use std::{collections::HashMap, path::PathBuf, sync::Mutex, time::Instant};

use serde::{Deserialize, Serialize};
use tauri::Manager;

const LIMIT: usize = 1024;
const BATCH: usize = 64;
const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const KINDS: &[&str] = &[
    "policy_decision",
    "stop_requested",
    "capture_started",
    "capture_source_ended",
    "capture_stopped",
    "capture_error",
];
const REASONS: &[&str] = &[
    "observed",
    "meeting_active",
    "ax_unknown",
    "ax_error",
    "mic_still_active",
    "network_recovery_hold",
    "user_kept_recording",
    "auto_stop_disabled",
    "auto_start_disabled",
    "identity_unavailable",
    "owner_mismatch",
    "account_changed",
    "manual_stop",
    "sleep",
    "silence_timeout",
    "grace_pending",
    "grace_elapsed",
    "native_lifecycle",
    "native_capture_ended",
    "probe_error",
    "probe_timeout",
    "stop_failed",
    "stop_prompt_pending",
    "stop_prompt_disabled",
    "saving_failed",
    "collector_started",
    "start_requested",
    "start_failed",
    "frontend_start_failed",
    "frontend_start_requested",
    "collector_attached_late",
    "audio_capture_error",
    "transcription_error",
    "sharing_lookup_pending",
    "sharing_lookup_failed",
    "delivery_failed",
    "recording_excluded",
];

pub type Evidence = std::collections::BTreeMap<String, serde_json::Value>;

fn evidence_valid(evidence: &Evidence) -> bool {
    evidence.iter().all(|(key, value)| match key.as_str() {
        "observation" => value
            .as_str()
            .is_some_and(|value| ["active", "inactive", "unknown"].contains(&value)),
        "inspectionStatus" | "micStatus" => value.as_str().is_some_and(|value| {
            ["ok", "error", "timeout", "pending", "unavailable"].contains(&value)
        }),
        "osVersion" => value.as_str().is_some_and(|version| {
            !version.is_empty()
                && version.len() <= 32
                && version.bytes().all(|c| c.is_ascii_digit() || c == b'.')
        }),
        "os" => value
            .as_str()
            .is_some_and(|value| ["macos", "linux", "windows", "unknown"].contains(&value)),
        "micActive" | "autoStop" | "autoStart" | "identityReady" | "online" | "owned"
        | "speakerRecent" | "retentionDropped" => value.is_boolean(),
        "missingForMs" | "networkHoldRemainingMs" | "probeElapsedMs" => {
            value.as_u64().is_some_and(|value| value <= 86_400_000)
        }
        "droppedCount" => value.as_u64().is_some_and(|value| value <= 1_000_000_000),
        _ => false,
    })
}

#[derive(Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    pub session_id: Option<String>,
    pub kind: String,
    pub reason: String,
    #[serde(default)]
    pub evidence: Evidence,
}

impl Input {
    fn validate(&self) -> Result<(), String> {
        if !KINDS.contains(&self.kind.as_str()) || !REASONS.contains(&self.reason.as_str()) {
            return Err("Unsupported diagnostic event".into());
        }
        if self
            .session_id
            .as_ref()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
        {
            return Err("Invalid diagnostic session".into());
        }
        if !evidence_valid(&self.evidence) {
            return Err("Unsupported diagnostic evidence".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticEvent {
    pub id: String,
    pub device_id: String,
    pub boot_id: String,
    pub sequence: u64,
    pub observed_at_ms: i64,
    pub elapsed_ms: u64,
    pub app_version: String,
    pub build_sha: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub reason: String,
    pub evidence: Evidence,
}

impl DiagnosticEvent {
    fn valid(&self, device: &str) -> bool {
        self.device_id == device
            && uuid::Uuid::parse_str(&self.id).is_ok()
            && uuid::Uuid::parse_str(&self.boot_id).is_ok()
            && self.sequence <= i64::MAX as u64
            && self.elapsed_ms <= i64::MAX as u64
            && self.observed_at_ms > 0
            && self.observed_at_ms <= now_ms().saturating_add(300_000)
            && !self.app_version.is_empty()
            && self.app_version.len() <= 64
            && self.app_version.as_bytes()[0].is_ascii_alphanumeric()
            && self
                .app_version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".+_-".contains(&c))
            && (self.build_sha == "unknown"
                || ((7..=64).contains(&self.build_sha.len())
                    && self.build_sha.bytes().all(|c| c.is_ascii_hexdigit())))
            && Input {
                session_id: self.session_id.clone(),
                kind: self.kind.clone(),
                reason: self.reason.clone(),
                evidence: self.evidence.clone(),
            }
            .validate()
            .is_ok()
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    device_id: String,
    sequence: u64,
    events: Vec<DiagnosticEvent>,
    dropped_count: u64,
    retention_dropped: bool,
}

impl Journal {
    fn prune(&mut self) {
        let before = self.events.len();
        self.events
            .retain(|event| event.observed_at_ms >= now_ms().saturating_sub(RETENTION_MS));
        self.retention_dropped |= self.events.len() < before;
        let overflow = self.events.len().saturating_sub(LIMIT);
        self.events.drain(..overflow);
        self.dropped_count = self
            .dropped_count
            .saturating_add((before - self.events.len()) as u64);
    }
}

#[derive(Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBatch {
    pub device_id: String,
    pub boot_id: String,
    pub events: Vec<DiagnosticEvent>,
    pub dropped_count: u64,
    pub retention_dropped: bool,
}

pub struct RecordingDiagnostics(Mutex<Collector>);

struct Collector {
    directory: Option<PathBuf>,
    user_id: Option<String>,
    owners: HashMap<String, String>,
    boot_id: String,
    started: Instant,
    // Timestamp observation before queued disk work can delay it.
    observed: (i64, Instant),
    app_version: String,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn cleanup(
    directory: Option<&std::path::Path>,
    expired_before: Option<std::time::SystemTime>,
) -> Result<(), String> {
    let Some(directory) = directory else {
        return Ok(());
    };
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("Diagnostics cleanup failed".into()),
    };
    for entry in entries {
        let entry = entry.map_err(|_| "Diagnostics cleanup failed")?;
        let path = entry.path();
        let owned = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
            && matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("json" | "tmp")
            );
        if owned
            && entry.file_type().is_ok_and(|kind| kind.is_file())
            && expired_before.is_none_or(|cutoff| {
                entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .is_ok_and(|modified| modified < cutoff)
            })
        {
            std::fs::remove_file(path).map_err(|_| "Diagnostics cleanup failed")?;
        }
    }
    Ok(())
}

impl RecordingDiagnostics {
    pub fn new(directory: Option<PathBuf>, app_version: String) -> Self {
        let cutoff =
            std::time::SystemTime::now() - std::time::Duration::from_millis(RETENTION_MS as u64);
        if cleanup(directory.as_deref(), Some(cutoff)).is_err() {
            tracing::warn!("recording_diagnostics_cleanup_failed");
        }
        let started = Instant::now();
        Self(Mutex::new(Collector {
            directory,
            user_id: None,
            owners: HashMap::new(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            started,
            observed: (now_ms(), started),
            app_version,
        }))
    }
}

impl Collector {
    fn path(&self, user: &str) -> Result<PathBuf, String> {
        let user = uuid::Uuid::parse_str(user).map_err(|_| "Invalid diagnostic account")?;
        Ok(self
            .directory
            .as_ref()
            .ok_or("Diagnostics storage unavailable")?
            .join(format!("{user}.json")))
    }

    fn load(&self, user: &str) -> Result<Journal, String> {
        let path = self.path(user)?;
        let mut journal = match std::fs::metadata(&path) {
            Ok(meta) if meta.len() <= MAX_FILE_BYTES => serde_json::from_slice::<Journal>(
                &std::fs::read(path).map_err(|_| "Diagnostics read failed")?,
            )
            .map_err(|_| "Invalid diagnostic journal")?,
            Ok(_) => return Err("Diagnostic journal exceeds size limit".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Journal {
                device_id: uuid::Uuid::new_v4().to_string(),
                ..Default::default()
            },
            Err(_) => return Err("Diagnostics read failed".into()),
        };
        if uuid::Uuid::parse_str(&journal.device_id).is_err() {
            return Err("Invalid diagnostic device".into());
        }
        let before = journal.events.len();
        journal
            .events
            .retain(|event| event.valid(&journal.device_id));
        journal.dropped_count = journal
            .dropped_count
            .saturating_add((before - journal.events.len()) as u64);
        journal.prune();
        Ok(journal)
    }

    fn save(&self, user: &str, journal: &Journal) -> Result<(), String> {
        use std::io::Write;
        let path = self.path(user)?;
        let parent = path.parent().ok_or("Diagnostics storage unavailable")?;
        std::fs::create_dir_all(parent).map_err(|_| "Diagnostics storage unavailable")?;
        let temporary = path.with_extension("tmp");
        let mut file = std::fs::File::create(&temporary).map_err(|_| "Diagnostics write failed")?;
        file.write_all(
            &serde_json::to_vec(journal).map_err(|_| "Diagnostics serialization failed")?,
        )
        .and_then(|_| file.sync_all())
        .map_err(|_| "Diagnostics write failed")?;
        std::fs::rename(temporary, &path).map_err(|_| "Diagnostics write failed")?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|_| "Diagnostics write failed")?;
        Ok(())
    }

    fn append(&self, user: &str, mut input: Input) -> Result<(), String> {
        let os = match std::env::consts::OS {
            "macos" => "macos",
            "linux" => "linux",
            "windows" => "windows",
            _ => "unknown",
        };
        input
            .evidence
            .insert("os".into(), serde_json::Value::String(os.into()));
        input.validate()?;
        if input
            .session_id
            .as_ref()
            .and_then(|id| self.owners.get(id))
            .is_some_and(|owner| owner != user)
        {
            return Err("Diagnostic session belongs to another account".into());
        }
        let mut journal = self.load(user)?;
        journal.sequence = journal.sequence.saturating_add(1).min(i64::MAX as u64);
        journal.events.push(DiagnosticEvent {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: journal.device_id.clone(),
            boot_id: self.boot_id.clone(),
            sequence: journal.sequence,
            observed_at_ms: self.observed.0,
            elapsed_ms: self
                .observed
                .1
                .saturating_duration_since(self.started)
                .as_millis()
                .min(i64::MAX as u128) as u64,
            app_version: self.app_version.clone(),
            build_sha: env!("ANLG_DIAGNOSTICS_BUILD_SHA").into(),
            session_id: input.session_id,
            kind: input.kind,
            reason: input.reason,
            evidence: input.evidence,
        });
        journal.prune();
        self.save(user, &journal)
    }

    fn configure(
        &mut self,
        user: Option<String>,
        enabled: bool,
        purge: bool,
    ) -> Result<(), String> {
        if enabled {
            self.user_id = None;
            let user = user.ok_or("Diagnostic account required")?;
            let journal = self.load(&user)?;
            self.save(&user, &journal)?;
            self.user_id = Some(user);
        } else {
            if user.is_none() || self.user_id == user {
                self.user_id = None;
            }
            if purge {
                self.owners
                    .retain(|_, owner| user.as_ref().is_some_and(|user| user != owner));
                if let Some(user) = user {
                    let path = self.path(&user)?;
                    for path in [path.clone(), path.with_extension("tmp")] {
                        match std::fs::remove_file(path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(_) => return Err("Diagnostics removal failed".into()),
                        }
                    }
                } else {
                    cleanup(self.directory.as_deref(), None)?;
                }
            }
        }
        Ok(())
    }

    fn require_user(&self, user: &str) -> Result<(), String> {
        if self.user_id.as_deref() == Some(user) {
            Ok(())
        } else {
            Err("Diagnostics account inactive".into())
        }
    }

    fn begin(&mut self, session: &str, user: Option<&str>, reason: &str) -> Result<(), String> {
        let Some(user) = user.filter(|user| self.user_id.as_deref() == Some(*user)) else {
            return Ok(());
        };
        if self.owners.get(session).is_some_and(|owner| owner != user) {
            return Err("Diagnostic session belongs to another account".into());
        }
        if self.owners.contains_key(session) && reason == "collector_attached_late" {
            return Ok(());
        }
        self.owners
            .entry(session.into())
            .or_insert_with(|| user.into());
        self.native(session, "policy_decision", reason)
    }

    fn native(&mut self, session: &str, kind: &str, reason: &str) -> Result<(), String> {
        let Some(owner) = self.owners.get(session).cloned() else {
            return Ok(());
        };
        let result = self.append(
            &owner,
            Input {
                session_id: Some(session.into()),
                kind: kind.into(),
                reason: reason.into(),
                evidence: Evidence::default(),
            },
        );
        if kind == "capture_stopped" {
            self.owners.remove(session);
        }
        result
    }

    fn start_failed(&mut self, session: &str, capture_active: bool) -> Result<(), String> {
        let result = self.native(session, "capture_error", "start_failed");
        if !capture_active {
            self.owners.remove(session);
        }
        result
    }

    fn batch(&self, user: &str) -> Result<DiagnosticBatch, String> {
        self.require_user(user)?;
        let journal = self.load(user)?;
        Ok(DiagnosticBatch {
            device_id: journal.device_id,
            boot_id: self.boot_id.clone(),
            events: journal.events.into_iter().take(BATCH).collect(),
            dropped_count: journal.dropped_count,
            retention_dropped: journal.retention_dropped,
        })
    }

    fn ack(&self, user: &str, ids: &[String]) -> Result<(), String> {
        self.require_user(user)?;
        if ids.len() > BATCH {
            return Err("Diagnostic acknowledgement exceeds batch limit".into());
        }
        let mut journal = self.load(user)?;
        journal.events.retain(|event| !ids.contains(&event.id));
        self.save(user, &journal)
    }
}

fn with_collector<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    action: impl FnOnce(&mut Collector) -> Result<(), String>,
) {
    let observed = (now_ms(), Instant::now());
    if let Some(state) = app.try_state::<RecordingDiagnostics>()
        && let Ok(mut collector) = state.0.lock()
    {
        collector.observed = observed;
        if action(&mut collector).is_err() {
            tracing::warn!("recording_diagnostics_write_failed");
        }
    }
}

pub fn begin<R: tauri::Runtime>(app: &tauri::AppHandle<R>, session: &str, user: Option<&str>) {
    with_collector(app, |collector| {
        collector.begin(session, user, "start_requested")
    });
}

pub fn start_failed<R: tauri::Runtime>(app: &tauri::AppHandle<R>, session: &str) {
    let active = ractor::registry::where_is(
        anlg_transcription_core::listener::actors::SourceActor::name(session),
    )
    .is_some();
    with_collector(app, |collector| collector.start_failed(session, active));
}

pub fn lifecycle(app: &tauri::AppHandle, session: &str, kind: &str, reason: &str) {
    with_collector(app, |collector| {
        if kind == "capture_started" {
            let owners = crate::shared_recording::diagnostic_capture_owners(app);
            if collector
                .owners
                .get(session)
                .is_some_and(|owner| owners.get(session) != Some(owner))
            {
                return Err("Diagnostic capture owner changed".into());
            }
            collector.begin(
                session,
                owners.get(session).map(String::as_str),
                "collector_attached_late",
            )?;
        }
        collector.native(session, kind, reason)
    });
}

pub fn stopped(app: &tauri::AppHandle, session: &str, has_error: bool) {
    with_collector(app, |collector| {
        let error = if has_error {
            collector
                .native(session, "capture_error", "native_lifecycle")
                .err()
        } else {
            None
        };
        collector
            .native(session, "capture_stopped", "native_lifecycle")
            .and(error.map_or(Ok(()), Err))
    });
}

async fn run_command<T: Send + 'static>(
    app: tauri::AppHandle,
    action: impl FnOnce(&mut Collector) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let observed = (now_ms(), Instant::now());
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<RecordingDiagnostics>();
        let mut collector = state.0.lock().map_err(|_| "Diagnostics unavailable")?;
        collector.observed = observed;
        action(&mut collector)
    })
    .await
    .map_err(|_| "Diagnostics unavailable".to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn recording_diagnostics_configure(
    app: tauri::AppHandle,
    user_id: Option<String>,
    enabled: bool,
    purge: Option<bool>,
) -> Result<(), String> {
    let handle = app.clone();
    run_command(app, move |collector| {
        collector.configure(user_id, enabled, purge.unwrap_or(false))?;
        if enabled {
            for (session, owner) in crate::shared_recording::diagnostic_capture_owners(&handle) {
                if ractor::registry::where_is(
                    anlg_transcription_core::listener::actors::SourceActor::name(&session),
                )
                .is_some()
                {
                    collector.begin(&session, Some(&owner), "collector_attached_late")?;
                }
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn recording_diagnostics_record(
    app: tauri::AppHandle,
    user_id: String,
    input: Input,
) -> Result<(), String> {
    run_command(app, move |collector| {
        collector.require_user(&user_id)?;
        collector.append(&user_id, input)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn recording_diagnostics_batch(
    app: tauri::AppHandle,
    user_id: String,
) -> Result<DiagnosticBatch, String> {
    run_command(app, move |collector| collector.batch(&user_id)).await
}

#[tauri::command]
#[specta::specta]
pub async fn recording_diagnostics_ack(
    app: tauri::AppHandle,
    user_id: String,
    event_ids: Vec<String>,
) -> Result<(), String> {
    run_command(app, move |collector| collector.ack(&user_id, &event_ids)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collector(path: PathBuf) -> Collector {
        RecordingDiagnostics::new(Some(path), "1.0.0".into())
            .0
            .into_inner()
            .unwrap()
    }

    #[test]
    fn offline_outbox_survives_restart_and_ack_only_removes_confirmed_events() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let user = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        let clock = Instant::now();
        state.started = clock - std::time::Duration::from_secs(3);
        state.observed = (now_ms() - 2000, clock - std::time::Duration::from_secs(2));
        state.configure(Some(user.clone()), true, false).unwrap();
        state
            .begin(&session, Some(&user), "start_requested")
            .unwrap();
        state
            .native(&session, "capture_stopped", "native_lifecycle")
            .unwrap();
        let first = state.batch(&user).unwrap();
        assert_eq!(first.events[0].observed_at_ms, state.observed.0);
        assert_eq!(first.events[0].elapsed_ms, 1000);
        let mut restored = collector(dir.clone());
        restored.configure(Some(user.clone()), true, false).unwrap();
        assert_eq!(restored.batch(&user).unwrap().device_id, first.device_id);
        restored.ack(&user, &[first.events[0].id.clone()]).unwrap();
        restored.ack(&user, &[first.events[0].id.clone()]).unwrap();
        assert_eq!(
            restored.batch(&user).unwrap().events[0].id,
            first.events[1].id
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_terminal_events_keep_original_owner_and_opt_out_prevents_recreation() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        state.begin(&session, Some(&a), "start_requested").unwrap();
        assert!(state.owners.is_empty());
        state.configure(Some(a.clone()), true, false).unwrap();
        state
            .begin(&session, Some(&b), "collector_attached_late")
            .unwrap();
        assert!(state.owners.is_empty());
        state
            .begin(&session, Some(&a), "collector_attached_late")
            .unwrap();
        state.begin(&session, Some(&a), "start_requested").unwrap();
        state.start_failed(&session, true).unwrap();
        assert_eq!(state.owners.get(&session), Some(&a));
        state.configure(Some(b.clone()), true, false).unwrap();
        assert!(state.ack(&a, &[]).is_err());
        state
            .native(&session, "capture_stopped", "native_lifecycle")
            .unwrap();
        assert!(state.batch(&b).unwrap().events.is_empty());
        state.configure(Some(a.clone()), true, false).unwrap();
        assert_eq!(state.batch(&a).unwrap().events.len(), 4);
        state.begin(&session, Some(&a), "start_requested").unwrap();
        state.configure(Some(a.clone()), false, true).unwrap();
        state
            .native(&session, "capture_stopped", "native_lifecycle")
            .unwrap();
        state.configure(Some(a.clone()), true, false).unwrap();
        assert!(state.batch(&a).unwrap().events.is_empty());
        state.configure(None, false, true).unwrap();
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn expired_and_invalid_events_cannot_poison_or_grow_the_outbox() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let user = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        state.configure(Some(user.clone()), true, false).unwrap();
        state
            .begin(&session, Some(&user), "start_requested")
            .unwrap();
        let mut journal = state.load(&user).unwrap();
        let event = journal.events[0].clone();
        journal.events = vec![event; LIMIT + 4];
        journal.events[0].id = "invalid".into();
        journal.events[1].observed_at_ms = now_ms() - RETENTION_MS - 1;
        state.save(&user, &journal).unwrap();
        let batch = state.batch(&user).unwrap();
        assert_eq!(state.load(&user).unwrap().events.len(), LIMIT);
        assert_eq!(batch.events.len(), BATCH);
        assert_eq!(batch.dropped_count, 4);
        assert!(batch.retention_dropped);
        let input = Input {
            session_id: None,
            kind: "policy_decision".into(),
            reason: "observed".into(),
            evidence: Evidence::from([("transcript".into(), serde_json::json!("private"))]),
        };
        assert!(state.append(&user, input).is_err());
        let stale = dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let old = std::time::SystemTime::now()
            - std::time::Duration::from_millis(RETENTION_MS as u64 + 1000);
        std::fs::File::create(&stale)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        let unrelated = dir.join("unrelated.json");
        std::fs::write(&unrelated, b"keep").unwrap();
        let _ = collector(dir.clone());
        assert!(!stale.exists());
        assert!(unrelated.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
