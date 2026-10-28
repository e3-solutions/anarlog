use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Mutex,
    time::Instant,
};

use serde::{Deserialize, Serialize};
use tauri::Manager;

const LIMIT: usize = 1024;
const BATCH: usize = 64;
const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const KINDS: &[&str] = &[
    "detector_observation",
    "policy_decision",
    "stop_requested",
    "capture_started",
    "capture_finalizing",
    "capture_source_ended",
    "capture_stopped",
    "recovery_interrupted",
    "capture_error",
];
const REASONS: &[&str] = &[
    "observed",
    "meeting_active",
    "meeting_ended",
    "ax_unknown",
    "ax_error",
    "ax_probe_timeout",
    "mic_still_active",
    "mic_inactive",
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
    "process_interrupted",
    "probe_error",
    "probe_timeout",
    "probe_started",
    "stop_failed",
    "saved_audio_ready",
    "saving_failed",
    "collector_started",
    "collector_attached_late",
    "audio_capture_error",
    "transcription_error",
    "sharing_lookup_pending",
    "sharing_lookup_failed",
    "delivery_failed",
    "recording_excluded",
];

#[derive(Clone, Default, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Evidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspection_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mic_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mic_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_stop: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_start: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_recent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_for_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_hold_remaining_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_dropped: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ack_age_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leave_control_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leave_control_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leave_control_visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_actor_present: Option<bool>,
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
        let e = &self.evidence;
        if e.matched_by
            .as_ref()
            .is_some_and(|v| !["context", "toolbar", "none"].contains(&v.as_str()))
        {
            return Err("Unsupported diagnostic match".into());
        }
        if e.platform
            .as_ref()
            .is_some_and(|v| !["slack", "googleMeet", "unknown"].contains(&v.as_str()))
            || e.os
                .as_ref()
                .is_some_and(|v| !["macos", "linux", "windows", "unknown"].contains(&v.as_str()))
        {
            return Err("Unsupported diagnostic platform".into());
        }
        if e.observation
            .as_ref()
            .is_some_and(|v| !["active", "inactive", "unknown"].contains(&v.as_str()))
            || [&e.inspection_status, &e.mic_status]
                .into_iter()
                .flatten()
                .any(|v| {
                    !["ok", "error", "timeout", "pending", "unavailable"].contains(&v.as_str())
                })
            || [
                e.missing_for_ms,
                e.network_hold_remaining_ms,
                e.probe_elapsed_ms,
            ]
            .into_iter()
            .flatten()
            .any(|v| v > 86_400_000)
            || e.last_ack_age_ms.is_some_and(|v| v > 86_400_000)
            || [e.queued_count, e.dropped_count]
                .into_iter()
                .flatten()
                .any(|v| v > 1_000_000_000)
        {
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
    fn is_witness(&self) -> bool {
        (self.kind == "detector_observation"
            && matches!(
                self.reason.as_str(),
                "observed" | "probe_started" | "native_lifecycle"
            ))
            || (self.kind == "policy_decision" && self.reason == "collector_started")
    }

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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    device_id: String,
    sequence: u64,
    events: Vec<DiagnosticEvent>,
    dropped_count: u64,
    last_ack_at_ms: Option<i64>,
    retention_dropped: bool,
    active_sessions: HashMap<String, String>,
}

#[derive(Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticBatch {
    pub device_id: String,
    pub boot_id: String,
    pub events: Vec<DiagnosticEvent>,
    pub dropped_count: u64,
    pub retention_dropped: bool,
    pub last_ack_at_ms: Option<i64>,
    pub queued_count: usize,
}

pub struct RecordingDiagnostics(Mutex<Collector>);

struct Collector {
    directory: Option<PathBuf>,
    user_id: Option<String>,
    journal: Journal,
    owners: HashMap<String, Option<String>>,
    startup_pending: HashSet<String>,
    initializing: bool,
    boot_id: String,
    started: Instant,
    app_version: String,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn prune_expired_files(directory: Option<&std::path::Path>) {
    let Some(directory) = directory else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_millis(RETENTION_MS as u64);
    for entry in entries.flatten() {
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
            && entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| modified < cutoff)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl RecordingDiagnostics {
    pub fn new(directory: Option<PathBuf>, app_version: String) -> Self {
        prune_expired_files(directory.as_deref());
        Self(Mutex::new(Collector {
            directory,
            user_id: None,
            journal: Journal::default(),
            owners: HashMap::new(),
            startup_pending: HashSet::new(),
            initializing: true,
            boot_id: uuid::Uuid::new_v4().to_string(),
            started: Instant::now(),
            app_version,
        }))
    }
}

impl Collector {
    fn path(&self, user_id: &str) -> Result<PathBuf, String> {
        let id = uuid::Uuid::parse_str(user_id).map_err(|_| "Invalid diagnostic account")?;
        Ok(self
            .directory
            .as_ref()
            .ok_or("Diagnostics storage unavailable")?
            .join(format!("{id}.json")))
    }

    fn persist(&self) -> Result<(), String> {
        let Some(user) = &self.user_id else {
            return Ok(());
        };
        let path = self.path(user)?;
        let parent = path.parent().ok_or("Diagnostics storage unavailable")?;
        std::fs::create_dir_all(parent).map_err(|_| "Diagnostics storage unavailable")?;
        let temp = path.with_extension("tmp");
        let mut file = std::fs::File::create(&temp).map_err(|_| "Diagnostics write failed")?;
        use std::io::Write;
        file.write_all(
            &serde_json::to_vec(&self.journal).map_err(|_| "Diagnostics serialization failed")?,
        )
        .and_then(|_| file.sync_all())
        .map_err(|_| "Diagnostics write failed")?;
        std::fs::rename(temp, &path).map_err(|_| "Diagnostics write failed")?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| "Diagnostics write failed")?;
        Ok(())
    }

    fn prune(&mut self) {
        let cutoff = now_ms().saturating_sub(RETENTION_MS);
        let before = self.journal.events.len();
        self.journal.events.retain(|e| e.observed_at_ms >= cutoff);
        let expired = before - self.journal.events.len();
        self.journal.retention_dropped |= expired > 0;
        let overflow = self.journal.events.len().saturating_sub(LIMIT);
        let mut remaining = overflow;
        self.journal.events.retain(|event| {
            if remaining > 0 && event.is_witness() {
                remaining -= 1;
                false
            } else {
                true
            }
        });
        self.journal.events.drain(..remaining);
        self.journal.dropped_count = self
            .journal
            .dropped_count
            .saturating_add((expired + overflow) as u64);
    }

    fn append(&mut self, mut input: Input) -> Result<(), String> {
        input.evidence.os = Some(
            match std::env::consts::OS {
                "macos" => "macos",
                "linux" => "linux",
                "windows" => "windows",
                _ => "unknown",
            }
            .into(),
        );
        input.validate()?;
        if self.user_id.is_none() {
            return Err("Diagnostics disabled".into());
        }
        if input
            .session_id
            .as_ref()
            .and_then(|id| self.owners.get(id))
            .is_some_and(|owner| owner != &self.user_id)
        {
            return Err("Diagnostic session belongs to another account".into());
        }
        self.journal.sequence = self.journal.sequence.saturating_add(1).min(i64::MAX as u64);
        self.journal.events.push(DiagnosticEvent {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.journal.device_id.clone(),
            boot_id: self.boot_id.clone(),
            sequence: self.journal.sequence,
            observed_at_ms: now_ms(),
            elapsed_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            app_version: self.app_version.clone(),
            build_sha: env!("ANLG_DIAGNOSTICS_BUILD_SHA").to_string(),
            session_id: input.session_id,
            kind: input.kind,
            reason: input.reason,
            evidence: input.evidence,
        });
        self.prune();
        self.persist()
    }

    fn configure(
        &mut self,
        user_id: Option<String>,
        enabled: bool,
        purge: bool,
    ) -> Result<(), String> {
        self.initializing = false;
        if !enabled || user_id.is_none() {
            self.startup_pending.clear();
            if purge {
                self.owners
                    .retain(|_, owner| user_id.is_some() && owner != &user_id);
            }
            if user_id.is_none() || self.user_id == user_id {
                self.user_id = None;
                self.journal = Journal::default();
            }
            if purge && let Some(user) = &user_id {
                let path = self.path(user)?;
                for path in [path.clone(), path.with_extension("tmp")] {
                    match std::fs::remove_file(path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => return Err("Diagnostics removal failed".into()),
                    }
                }
            } else if purge && let Some(dir) = &self.directory {
                match std::fs::remove_dir_all(dir) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err("Diagnostics removal failed".into()),
                }
            }
            return Ok(());
        }
        let user = user_id.as_ref().unwrap();
        if self.user_id == user_id {
            return Ok(());
        }
        self.user_id = None;
        self.journal = Journal::default();
        let path = self.path(user)?;
        let mut journal = if path.exists() {
            if std::fs::metadata(&path)
                .map_err(|_| "Diagnostics read failed")?
                .len()
                > MAX_FILE_BYTES
            {
                return Err("Diagnostics journal exceeds size limit".into());
            }
            serde_json::from_slice::<Journal>(
                &std::fs::read(path).map_err(|_| "Diagnostics read failed")?,
            )
            .map_err(|_| "Diagnostics journal is invalid")?
        } else {
            Journal {
                device_id: uuid::Uuid::new_v4().to_string(),
                ..Default::default()
            }
        };
        if uuid::Uuid::parse_str(&journal.device_id).is_err() {
            return Err("Diagnostics journal is invalid".into());
        }
        let before = journal.events.len();
        journal.events.retain(|e| e.valid(&journal.device_id));
        journal.dropped_count = journal
            .dropped_count
            .saturating_add((before - journal.events.len()) as u64);
        journal.active_sessions.retain(|id, boot| {
            uuid::Uuid::parse_str(id).is_ok() && uuid::Uuid::parse_str(boot).is_ok()
        });
        journal.sequence = journal.sequence.min(i64::MAX as u64);
        let interrupted = journal
            .active_sessions
            .iter()
            .filter(|(_, boot)| *boot != &self.boot_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in &interrupted {
            journal.active_sessions.remove(id);
        }
        self.user_id = user_id;
        self.journal = journal;
        self.prune();
        for id in interrupted {
            self.append(Input {
                session_id: Some(id),
                kind: "recovery_interrupted".into(),
                reason: "process_interrupted".into(),
                evidence: Evidence::default(),
            })?;
        }
        self.persist()
    }

    fn require_user(&self, user: &str) -> Result<(), String> {
        if self.user_id.as_deref() == Some(user) {
            Ok(())
        } else {
            Err("Diagnostics account inactive".into())
        }
    }

    fn batch(&mut self, user: &str) -> Result<DiagnosticBatch, String> {
        self.require_user(user)?;
        self.prune();
        self.persist()?;
        Ok(DiagnosticBatch {
            device_id: self.journal.device_id.clone(),
            boot_id: self.boot_id.clone(),
            events: self.journal.events.iter().take(BATCH).cloned().collect(),
            dropped_count: self.journal.dropped_count,
            retention_dropped: self.journal.retention_dropped,
            last_ack_at_ms: self.journal.last_ack_at_ms,
            queued_count: self.journal.events.len(),
        })
    }

    fn ack(&mut self, user: &str, ids: &[String]) -> Result<(), String> {
        self.require_user(user)?;
        if ids.len() > BATCH {
            return Err("Diagnostic acknowledgement exceeds batch limit".into());
        }
        let before = self.journal.events.len();
        self.journal.events.retain(|event| !ids.contains(&event.id));
        if before != self.journal.events.len() {
            self.journal.last_ack_at_ms = Some(now_ms());
        }
        self.persist()
    }

    fn native_event(&mut self, session_id: &str, kind: &str, reason: &str) -> Result<(), String> {
        self.native_event_with_evidence(session_id, kind, reason, Evidence::default())
    }

    fn native_event_with_evidence(
        &mut self,
        session_id: &str,
        kind: &str,
        reason: &str,
        evidence: Evidence,
    ) -> Result<(), String> {
        if kind == "capture_started" {
            if self.initializing && self.user_id.is_none() && !self.owners.contains_key(session_id)
            {
                self.startup_pending.insert(session_id.into());
            }
            self.owners
                .entry(session_id.into())
                .or_insert_with(|| self.user_id.clone());
        }
        let Some(owner) = self.owners.get(session_id).cloned().flatten() else {
            if kind == "capture_stopped" {
                self.owners.remove(session_id);
                self.startup_pending.remove(session_id);
            }
            return Ok(());
        };
        let result = if self.user_id.as_ref() == Some(&owner) {
            self.append_native(session_id, kind, reason, evidence)
        } else {
            // Finish an already consented session in its original account's outbox.
            let mut detached = Collector {
                directory: self.directory.clone(),
                user_id: None,
                journal: Journal::default(),
                owners: HashMap::new(),
                startup_pending: HashSet::new(),
                initializing: false,
                boot_id: self.boot_id.clone(),
                started: self.started,
                app_version: self.app_version.clone(),
            };
            detached
                .configure(Some(owner), true, false)
                .and_then(|_| detached.append_native(session_id, kind, reason, evidence))
        };
        if kind == "capture_stopped" {
            self.owners.remove(session_id);
            self.startup_pending.remove(session_id);
        }
        result
    }

    fn attach_startup_sessions(
        &mut self,
        capture_owners: &HashMap<String, String>,
    ) -> Result<(), String> {
        let Some(user) = self.user_id.clone() else {
            return Ok(());
        };
        let sessions = self
            .startup_pending
            .iter()
            .filter(|id| {
                self.owners.get(*id) == Some(&None) && capture_owners.get(*id) == Some(&user)
            })
            .cloned()
            .collect::<Vec<_>>();
        for id in sessions {
            self.owners.insert(id.clone(), Some(user.clone()));
            self.startup_pending.remove(&id);
            self.journal
                .active_sessions
                .insert(id.clone(), self.boot_id.clone());
            self.append(Input {
                session_id: Some(id),
                kind: "policy_decision".into(),
                reason: "collector_attached_late".into(),
                evidence: Evidence {
                    owned: Some(true),
                    ..Default::default()
                },
            })?;
        }
        Ok(())
    }

    fn append_native(
        &mut self,
        session_id: &str,
        kind: &str,
        reason: &str,
        evidence: Evidence,
    ) -> Result<(), String> {
        if kind == "capture_started" {
            self.journal
                .active_sessions
                .insert(session_id.into(), self.boot_id.clone());
        } else if kind == "capture_stopped" {
            self.journal.active_sessions.remove(session_id);
        }
        self.append(Input {
            session_id: Some(session_id.into()),
            kind: kind.into(),
            reason: reason.into(),
            evidence,
        })
    }

    fn heartbeat(&mut self, source_present: impl Fn(&str) -> bool) {
        let sessions = self
            .owners
            .iter()
            .filter(|(_, owner)| owner.is_some())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in sessions {
            let evidence = Evidence {
                source_actor_present: Some(source_present(&id)),
                ..Default::default()
            };
            if self
                .native_event_with_evidence(
                    &id,
                    "detector_observation",
                    "native_lifecycle",
                    evidence,
                )
                .is_err()
            {
                tracing::warn!("recording_diagnostics_heartbeat_failed");
            }
        }
    }
}

pub fn spawn_heartbeat(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(30));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let app = app.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                if let Some(state) = app.try_state::<RecordingDiagnostics>()
                    && let Ok(mut collector) = state.0.lock()
                {
                    collector.heartbeat(|id| {
                        ractor::registry::where_is(
                            anlg_transcription_core::listener::actors::SourceActor::name(id),
                        )
                        .is_some()
                    });
                }
            })
            .await;
        }
    });
}

pub fn lifecycle(app: &tauri::AppHandle, session_id: &str, kind: &str, reason: &str) {
    let Some(state) = app.try_state::<RecordingDiagnostics>() else {
        return;
    };
    let Ok(mut collector) = state.0.lock() else {
        return;
    };
    if collector.native_event(session_id, kind, reason).is_err() {
        tracing::warn!("recording_diagnostics_write_failed");
    }
}

#[tauri::command]
#[specta::specta]
pub fn recording_diagnostics_configure(
    app: tauri::AppHandle,
    user_id: Option<String>,
    enabled: bool,
    purge: Option<bool>,
) -> Result<(), String> {
    let state = app.state::<RecordingDiagnostics>();
    let mut collector = state.0.lock().map_err(|_| "Diagnostics unavailable")?;
    let capture_owners = if enabled {
        crate::shared_recording::diagnostic_capture_owners(&app)
    } else {
        HashMap::new()
    };
    collector.configure(user_id, enabled, purge.unwrap_or(false))?;
    if enabled {
        collector.attach_startup_sessions(&capture_owners)?;
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn recording_diagnostics_record(
    app: tauri::AppHandle,
    user_id: String,
    input: Input,
) -> Result<(), String> {
    let state = app.state::<RecordingDiagnostics>();
    let mut collector = state.0.lock().map_err(|_| "Diagnostics unavailable")?;
    collector.require_user(&user_id)?;
    collector.append(input)
}

#[tauri::command]
#[specta::specta]
pub fn recording_diagnostics_batch(
    app: tauri::AppHandle,
    user_id: String,
) -> Result<DiagnosticBatch, String> {
    app.state::<RecordingDiagnostics>()
        .0
        .lock()
        .map_err(|_| "Diagnostics unavailable")?
        .batch(&user_id)
}

#[tauri::command]
#[specta::specta]
pub fn recording_diagnostics_ack(
    app: tauri::AppHandle,
    user_id: String,
    event_ids: Vec<String>,
) -> Result<(), String> {
    app.state::<RecordingDiagnostics>()
        .0
        .lock()
        .map_err(|_| "Diagnostics unavailable")?
        .ack(&user_id, &event_ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collector(directory: PathBuf) -> Collector {
        RecordingDiagnostics::new(Some(directory), "1.0.0".into())
            .0
            .into_inner()
            .unwrap()
    }

    fn observation() -> Input {
        Input {
            session_id: None,
            kind: "detector_observation".into(),
            reason: "observed".into(),
            evidence: Evidence::default(),
        }
    }

    #[test]
    fn offline_journal_survives_restart_and_ack_keeps_other_events() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let account = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let mut first = collector(dir.clone());
        first.configure(Some(account.clone()), true, false).unwrap();
        first.append(observation()).unwrap();
        first.append(observation()).unwrap();
        first
            .journal
            .active_sessions
            .insert(session.clone(), first.boot_id.clone());
        first.persist().unwrap();
        let old = first.batch(&account).unwrap();
        let mut second = collector(dir.clone());
        second
            .configure(Some(account.clone()), true, false)
            .unwrap();
        let restored = second.batch(&account).unwrap();
        assert_eq!(old.device_id, restored.device_id);
        assert_eq!(old.events[0].id, restored.events[0].id);
        assert!(
            restored.events.iter().any(
                |e| e.kind == "recovery_interrupted" && e.session_id.as_ref() == Some(&session)
            )
        );
        second.ack(&account, &[old.events[0].id.clone()]).unwrap();
        second.ack(&account, &[old.events[0].id.clone()]).unwrap();
        let mut third = collector(dir.clone());
        third.configure(Some(account.clone()), true, false).unwrap();
        let remaining = third.batch(&account).unwrap();
        assert_eq!(remaining.events.len(), 2);
        assert_eq!(remaining.events[0].id, old.events[1].id);
        assert!(remaining.last_ack_at_ms.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn account_switch_cannot_read_ack_or_reassign_an_existing_capture() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        state.configure(Some(a.clone()), true, false).unwrap();
        state.append(observation()).unwrap();
        state.owners.insert(session.clone(), Some(a.clone()));
        let original = state.batch(&a).unwrap();
        state.configure(Some(b.clone()), true, false).unwrap();
        assert!(state.batch(&a).is_err());
        assert!(state.ack(&a, &[original.events[0].id.clone()]).is_err());
        let mut event = observation();
        event.session_id = Some(session);
        assert!(state.append(event).is_err());
        assert!(state.batch(&b).unwrap().events.is_empty());
        state.configure(Some(a.clone()), false, false).unwrap();
        assert!(state.batch(&b).is_ok());
        state.configure(Some(a.clone()), true, false).unwrap();
        assert_eq!(state.batch(&a).unwrap().device_id, original.device_id);
        state.configure(Some(a.clone()), false, false).unwrap();
        state.configure(Some(a.clone()), true, false).unwrap();
        assert_eq!(state.batch(&a).unwrap().events[0].id, original.events[0].id);
        state.configure(None, false, true).unwrap();
        assert!(!dir.exists());
        assert!(state.append(observation()).is_err());
    }

    #[test]
    fn journal_bounds_offline_backlog_and_reports_retention_loss() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let account = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        state.configure(Some(account.clone()), true, false).unwrap();
        state.append(observation()).unwrap();
        let event = state.journal.events[0].clone();
        state.journal.events = vec![event; LIMIT + 9];
        state.prune();
        assert_eq!(state.journal.events.len(), LIMIT);
        assert_eq!(state.journal.dropped_count, 9);
        state.journal.events[0].observed_at_ms = now_ms() - RETENTION_MS - 1;
        state.persist().unwrap();
        let mut restored = collector(dir.clone());
        restored
            .configure(Some(account.clone()), true, false)
            .unwrap();
        let batch = restored.batch(&account).unwrap();
        assert_eq!(batch.events.len(), BATCH);
        assert_eq!(batch.dropped_count, 10);
        assert!(batch.retention_dropped);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_event_metadata_does_not_poison_later_delivery() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let account = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        state.configure(Some(account.clone()), true, false).unwrap();
        state.append(observation()).unwrap();
        state.append(observation()).unwrap();
        state.journal.events[0].id = "invalid".into();
        state.persist().unwrap();
        let mut restored = collector(dir.clone());
        restored
            .configure(Some(account.clone()), true, false)
            .unwrap();
        let batch = restored.batch(&account).unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.dropped_count, 1);
        assert_eq!(
            batch.events[0].evidence.os.as_deref(),
            Some(std::env::consts::OS)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminal_events_keep_start_owner_after_auth_loss_but_never_reappear_after_opt_out() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let session = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        state.configure(Some(a.clone()), true, false).unwrap();
        state
            .native_event(&session, "capture_started", "native_lifecycle")
            .unwrap();
        state.configure(Some(a.clone()), false, false).unwrap();
        state
            .native_event(&session, "capture_source_ended", "native_capture_ended")
            .unwrap();
        let unowned = uuid::Uuid::new_v4().to_string();
        state
            .native_event(&unowned, "capture_started", "native_lifecycle")
            .unwrap();
        state.configure(Some(b.clone()), true, false).unwrap();
        state
            .native_event(&session, "capture_stopped", "saved_audio_ready")
            .unwrap();
        state
            .native_event(&unowned, "capture_stopped", "native_lifecycle")
            .unwrap();
        assert!(state.batch(&b).unwrap().events.is_empty());
        state.configure(Some(a.clone()), true, false).unwrap();
        let batch = state.batch(&a).unwrap();
        assert_eq!(batch.events.len(), 3);
        assert_eq!(batch.events[1].kind, "capture_source_ended");
        assert_eq!(batch.events[2].kind, "capture_stopped");
        assert!(state.owners.is_empty());
        state
            .native_event(&session, "capture_started", "native_lifecycle")
            .unwrap();
        state.configure(Some(a.clone()), false, true).unwrap();
        state
            .native_event(&session, "capture_stopped", "native_lifecycle")
            .unwrap();
        state.configure(Some(a.clone()), true, false).unwrap();
        assert!(state.batch(&a).unwrap().events.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_heartbeat_only_observes_owned_pending_sessions_and_honors_opt_out() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let account = uuid::Uuid::new_v4().to_string();
        let owned = uuid::Uuid::new_v4().to_string();
        let unowned = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        state
            .native_event(&unowned, "capture_started", "native_lifecycle")
            .unwrap();
        state.configure(Some(account.clone()), true, false).unwrap();
        state
            .native_event(&owned, "capture_started", "native_lifecycle")
            .unwrap();
        state
            .configure(Some(account.clone()), false, false)
            .unwrap();
        state.heartbeat(|id| {
            assert_eq!(id, owned);
            false
        });
        state.configure(Some(account.clone()), true, false).unwrap();
        let events = state.batch(&account).unwrap().events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, "detector_observation");
        assert_eq!(events[1].evidence.source_actor_present, Some(false));
        state.configure(Some(account.clone()), false, true).unwrap();
        state.heartbeat(|_| panic!("Opted-out sessions must not be inspected"));
        state.configure(Some(account.clone()), true, false).unwrap();
        assert!(state.batch(&account).unwrap().events.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn late_configuration_attaches_only_verified_startup_owner_and_cannot_undo_opt_out() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let a = uuid::Uuid::new_v4().to_string();
        let b = uuid::Uuid::new_v4().to_string();
        let owned = uuid::Uuid::new_v4().to_string();
        let other = uuid::Uuid::new_v4().to_string();
        let unowned = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        for id in [&owned, &other, &unowned] {
            state
                .native_event(id, "capture_started", "native_lifecycle")
                .unwrap();
        }
        let verified = HashMap::from([(owned.clone(), a.clone()), (other.clone(), b)]);
        state.configure(Some(a.clone()), true, false).unwrap();
        state.attach_startup_sessions(&verified).unwrap();
        let events = state.batch(&a).unwrap().events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, "collector_attached_late");
        assert_eq!(events[0].session_id.as_ref(), Some(&owned));
        assert_eq!(state.owners.get(&other), Some(&None));
        assert_eq!(state.owners.get(&unowned), Some(&None));
        state
            .native_event(&owned, "capture_source_ended", "native_capture_ended")
            .unwrap();
        state.configure(Some(a.clone()), false, true).unwrap();
        state
            .native_event(&owned, "capture_started", "native_lifecycle")
            .unwrap();
        state.configure(Some(a.clone()), true, false).unwrap();
        state.attach_startup_sessions(&verified).unwrap();
        assert!(state.batch(&a).unwrap().events.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dormant_account_cleanup_only_removes_expired_owned_journals() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::time::SystemTime::now()
            - std::time::Duration::from_millis(RETENTION_MS as u64 + 60_000);
        let stale = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
        let partial = dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let unrelated = dir.join("unrelated.json");
        for path in [&stale, &partial, &unrelated] {
            std::fs::File::create(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        let recent = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&recent, b"recent").unwrap();
        prune_expired_files(Some(&dir));
        assert!(!stale.exists());
        assert!(!partial.exists());
        assert!(unrelated.exists());
        assert!(recent.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn offline_witness_bursts_cannot_crowd_out_stop_events_and_critical_only_backlogs_stay_bounded()
    {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let account = uuid::Uuid::new_v4().to_string();
        let mut state = collector(dir.clone());
        state.configure(Some(account.clone()), true, false).unwrap();
        let stop = Input {
            session_id: None,
            kind: "capture_stopped".into(),
            reason: "saved_audio_ready".into(),
            evidence: Evidence::default(),
        };
        state.append(stop.clone()).unwrap();
        let stop_id = state.journal.events[0].id.clone();
        state.append(observation()).unwrap();
        let witness = state.journal.events[1].clone();
        state.journal.events.extend(vec![witness; LIMIT]);
        state.append(observation()).unwrap();
        assert_eq!(state.journal.events.len(), LIMIT);
        assert!(state.journal.events.iter().any(|event| event.id == stop_id));
        assert_eq!(state.journal.dropped_count, 3);
        let critical = state.journal.events[0].clone();
        state.journal.events = vec![critical; LIMIT + 3];
        state.append(stop).unwrap();
        assert_eq!(state.journal.events.len(), LIMIT);
        assert!(
            state
                .journal
                .events
                .iter()
                .all(|event| event.kind == "capture_stopped")
        );
        assert_eq!(state.journal.dropped_count, 7);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
