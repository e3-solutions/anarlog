use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::Manager;
use tauri_plugin_fs_sync::FsSyncPluginExt;
use tauri_plugin_settings::SettingsPluginExt;

pub(crate) mod meeting_control;

const QUARANTINED_MESSAGE: &str = "Meeting activity could not be verified throughout this recording. Original audio stays local; start a new recording in a new note with one verified call to share.";

#[derive(Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub integration_id: String,
    pub connection_id: String,
    pub status: String,
}
#[derive(Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct Configuration {
    pub enabled: bool,
    pub api_base_url: String,
    pub organization_id: String,
    pub user_id: String,
    pub connections: Vec<Connection>,
}
impl Configuration {
    fn unique_connection(&self, integration: &str) -> Option<&Connection> {
        let mut connected = self.connections.iter().filter(|connection| {
            connection.status == "connected" && connection.integration_id == integration
        });
        let connection = connected.next()?;
        connected.next().is_none().then_some(connection)
    }
}
#[derive(Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub session_id: String,
    pub state: String,
    pub error: Option<String>,
    pub user_id: String,
    pub organization_id: String,
    #[serde(default)]
    device_id: String,
    api_base_url: String,
    transcript_id: String,
    started_at_ms: i64,
    stopped_at_ms: i64,
    binding: Option<Value>,
    segments: Vec<Segment>,
    #[serde(default)]
    gaps: Vec<Gap>,
    #[serde(default)]
    raw_frames: Option<u64>,
}
#[derive(Clone, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
struct Segment {
    sha256: String,
    frames: u64,
}
#[derive(Clone, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ObservationSummary {
    inspection_status: String,
    row_count: u32,
    active_call_count: u32,
    trusted_count: u32,
    warning_categories: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Binding {
    session_id: String,
    user_id: String,
    organization_id: String,
    api_base_url: String,
    admission: Value,
    observed_at_ms: i64,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    call_identity: Option<String>,
    #[serde(default)]
    verification_pending: bool,
    #[serde(default)]
    connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quarantine_reason: Option<String>,
}
impl Binding {
    fn valid_binding(&self) -> bool {
        !self.verification_pending && !self.admission.is_null()
    }
    fn quarantine(&mut self, reason: &str) {
        self.admission = Value::Null;
        let reason = self
            .quarantine_reason
            .get_or_insert_with(|| reason.into())
            .as_str();
        tracing::warn!(
            reason,
            session = key(&self.session_id),
            "shared_recording_quarantined"
        );
    }
}
#[derive(Clone, Serialize, Deserialize, specta::Type)]
struct Gap {
    start: u64,
    end: u64,
}
#[derive(Default)]
struct RawTimeline {
    frames: u64,
    gaps: Vec<Gap>,
    owner: Option<Configuration>,
    capture_user_id: Option<String>,
    started_at_ms: i64,
    storage_failed: bool,
    stopped_at_ms: Option<i64>,
}
impl RawTimeline {
    fn owned_by(&self, config: &Configuration) -> bool {
        self.owner.as_ref().is_some_and(|owner| {
            owner.user_id == config.user_id
                && owner.organization_id == config.organization_id
                && owner.api_base_url == config.api_base_url
        })
    }
}
#[derive(Default)]
pub(crate) struct QueueState {
    pub(crate) pins: Mutex<HashSet<String>>,
    serial: tokio::sync::Mutex<()>,
    // Quarantine cannot wait behind audio preparation or provider requests.
    bindings: Mutex<()>,
    observe_serial: tokio::sync::Mutex<()>,
    flush_serial: tokio::sync::Mutex<()>,
    raw: Mutex<HashMap<String, RawTimeline>>,
    excluded: Mutex<HashSet<String>>,
    control: Mutex<Option<tauri_plugin_tray::MeetingRecordingContext>>,
    holds: Mutex<HashMap<String, tauri_plugin_tray::MeetingRecordingContext>>,
    hold_errors: Mutex<HashSet<String>>,
    cancellations: Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
}

const EXCLUDED_MESSAGE: &str = "Recording was excluded. Resume intentionally in a new recording.";

fn exclusion_path(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!("excluded-{}.json", key(session)))
}

pub(crate) fn ensure_allowed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: &str,
) -> Result<(), String> {
    let state = app.state::<QueueState>();
    if !capture_allowed(app, session)
        || exclusion_path(&directory(app)?, session)
            .try_exists()
            .map_err(|e| e.to_string())?
    {
        state
            .excluded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.into());
        return Err(EXCLUDED_MESSAGE.into());
    }
    Ok(())
}

pub(crate) fn capture_allowed<R: tauri::Runtime>(app: &tauri::AppHandle<R>, session: &str) -> bool {
    !app.state::<QueueState>()
        .excluded
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(session)
}

fn cancellation(state: &QueueState, session: &str) -> tokio_util::sync::CancellationToken {
    state
        .cancellations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(session.into())
        .or_default()
        .clone()
}

fn exclude_session(
    dir: &Path,
    state: &QueueState,
    session_id: &str,
    user_id: &str,
) -> Result<(), String> {
    {
        let _binding = state.bindings.lock().unwrap_or_else(|e| e.into_inner());
        if user_id.is_empty() {
            return Err("Choose a signed-in account".into());
        }
        let binding_path = dir.join(format!("binding-{}.json", key(session_id)));
        let mut binding = read::<Binding>(&binding_path)?;
        let capture_user = state
            .raw
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .and_then(|timeline| timeline.capture_user_id.clone());
        if binding
            .as_ref()
            .is_some_and(|binding| binding.user_id != user_id)
            || capture_user
                .as_deref()
                .is_some_and(|owner| owner != user_id)
        {
            return Err("Recording belongs to another account".into());
        }
        let known_context = state
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|context| context.session_id == session_id && context.user_id == user_id);
        if binding.is_none() && capture_user.is_none() && !known_context {
            if let Some(config) = read::<Configuration>(&dir.join("configuration.json"))? {
                if config.user_id != user_id {
                    return Err("Recording belongs to another account".into());
                }
            }
        }
        // Block in memory before disk I/O so a persistence error still stops this run.
        state
            .excluded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string());
        cancellation(&state, &session_id).cancel();
        save(
            &exclusion_path(&dir, &session_id),
            &json!({"userId":user_id,"sessionId":session_id}),
        )?;
        // Existing releases already honor quarantined bindings on queued uploads.
        if let Some(binding) = binding.as_mut() {
            binding.quarantine("user_excluded");
            binding.verification_pending = false;
            save(&binding_path, binding)?;
        }
        Ok(())
    }
}

fn exclude_gate<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: &str,
    user: &str,
) -> Result<(), String> {
    let state = app.state::<QueueState>();
    let result = match directory(app) {
        Ok(dir) => exclude_session(&dir, &state, session, user),
        Err(error) => {
            let capture_owner = state
                .raw
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(session)
                .and_then(|timeline| timeline.capture_user_id.clone());
            let known_context = state
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|context| context.session_id == session && context.user_id == user);
            if capture_owner.as_deref() != Some(user) && !known_context {
                return Err("Recording ownership could not be verified".into());
            }
            state
                .excluded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(session.into());
            cancellation(&state, session).cancel();
            Err(error)
        }
    };
    if !capture_allowed(app, session) {
        meeting_control::cancel_transcriptions(app, session);
    }
    result
}

async fn shared_recording_exclude<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session_id: String,
    user_id: String,
) -> Result<(), String> {
    let state = app.state::<QueueState>();
    let persistence = exclude_gate(&app, &session_id, &user_id);
    if capture_allowed(&app, &session_id) {
        return persistence;
    }
    let current_session = state
        .control
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|context| context.session_id == session_id && context.user_id == user_id);
    let hold = if current_session {
        meeting_control::hold_for_session(&app, &session_id, &user_id).map(|_| ())
    } else {
        Ok(())
    };
    let expected = state
        .control
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .filter(|context| context.session_id == session_id && context.user_id == user_id);
    let stopped = meeting_control::stop_verified(&app, &session_id).await;
    let result = stopped.and(persistence).and(hold);
    if let Some(expected) = expected {
        meeting_control::finish_stop(&app, &expected, result.is_err());
    }
    result
}

pub(crate) fn storage_failed<R: tauri::Runtime>(app: &tauri::AppHandle<R>, session_id: &str) {
    if let Some(timeline) = app
        .state::<QueueState>()
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(session_id)
    {
        timeline.storage_failed = true;
    }
}
pub(crate) fn start_raw<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session_id: &str,
    owner_user_id: Option<&str>,
) -> bool {
    let state = app.state::<QueueState>();
    let _binding = state
        .bindings
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let owner = directory(app)
        .ok()
        .and_then(|d| {
            read::<Configuration>(&d.join("configuration.json"))
                .ok()
                .flatten()
        })
        .filter(|c| c.enabled && owner_user_id == Some(c.user_id.as_str()));
    let shared = owner.is_some();
    app.state::<QueueState>()
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            session_id.to_string(),
            RawTimeline {
                owner,
                capture_user_id: owner_user_id.map(str::to_string),
                started_at_ms: chrono::Utc::now().timestamp_millis(),
                ..Default::default()
            },
        );
    shared
}

pub(crate) fn diagnostic_capture_owners(app: &tauri::AppHandle) -> HashMap<String, String> {
    app.state::<QueueState>()
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, timeline)| timeline.stopped_at_ms.is_none())
        .filter_map(|(id, timeline)| {
            timeline
                .capture_user_id
                .as_ref()
                .map(|owner| (id.clone(), owner.clone()))
        })
        .collect()
}
pub(crate) fn observe_raw<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session_id: &str,
    frames: u64,
    captured: bool,
) {
    let state = app.state::<QueueState>();
    let mut map = state.raw.lock().unwrap_or_else(|e| e.into_inner());
    let timeline = map.entry(session_id.to_string()).or_default();
    let start = timeline.frames;
    timeline.frames = timeline.frames.saturating_add(frames);
    if !captured && frames > 0 {
        if let Some(last) = timeline.gaps.last_mut().filter(|g| g.end == start) {
            last.end = timeline.frames;
        } else {
            timeline.gaps.push(Gap {
                start,
                end: timeline.frames,
            });
        }
    }
}

fn directory<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("shared-recording-outbox");
    private_directory(&dir)?;
    Ok(dir)
}
fn private_directory(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_private_from(path, bytes)
}
fn copy_private(source: &Path, target: &Path) -> Result<(), String> {
    write_private_from(
        target,
        std::fs::File::open(source).map_err(|e| e.to_string())?,
    )
}
fn write_private_from(path: &Path, mut source: impl std::io::Read) -> Result<(), String> {
    let tmp = path.with_extension("pending");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut source, &mut file)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    write_private(path, &serde_json::to_vec(value).map_err(|e| e.to_string())?)
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn key(value: &str) -> String {
    digest(value.as_bytes())
}
fn validate_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|e| e.to_string())?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))))
    {
        return Err("Shared service requires HTTPS or a local loopback address".into());
    }
    Ok(url)
}
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}
async fn success(response: reqwest::Response) -> Result<reqwest::Response, String> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(format!(
            "Shared service returned HTTP {}",
            response.status().as_u16()
        ))
    }
}
async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("Shared service response exceeds the download limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("Shared service response exceeds the download limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn json_body(response: reqwest::Response) -> Result<Value, String> {
    serde_json::from_slice(&bounded_body(response, 1024 * 1024).await?).map_err(|e| e.to_string())
}
fn cleanup_uploaded_audio(dir: &Path, job: &Job) -> Result<(), String> {
    for path in std::iter::once(dir.join(&job.id).join("original.audio")).chain(
        (0..job.segments.len()).map(|sequence| dir.join(&job.id).join(format!("{sequence}.wav"))),
    ) {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}
fn finish_preparation(job: &mut Job) {
    if let Some(raw_frames) = job.raw_frames {
        let decoded_frames: u64 = job.segments.iter().map(|segment| segment.frames).sum();
        let Some(padding) = decoded_frames
            .checked_sub(raw_frames)
            .filter(|padding| *padding <= 16000)
        else {
            job.state = "blocked_timeline".into();
            job.error = Some("Stored audio length does not match captured frames. Original retained; timeline needs verification.".into());
            return;
        };
        // MP3 encoder/decoder padding has no verified sample origin. Cover every
        // possible shift instead of mislabeling a failed interval as captured.
        for gap in &mut job.gaps {
            gap.end = gap.end.saturating_add(padding).min(decoded_frames);
        }
    }
    job.state = if job.binding.is_some() {
        "pending"
    } else {
        "awaiting_identity"
    }
    .into();
}
fn bound(config: &Configuration, user: &str) -> Result<(), String> {
    if config.enabled && config.user_id == user {
        Ok(())
    } else {
        Err("Shared recording is disabled or belongs to another account".into())
    }
}
fn job_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(id).join("job.json")
}
fn quarantine_binding(
    dir: &Path,
    config: &Configuration,
    session_id: &str,
    platform: &str,
    observed_at_ms: i64,
    reason: &str,
) -> Result<(), String> {
    let path = dir.join(format!("binding-{}.json", key(session_id)));
    let mut binding = read::<Binding>(&path)?.unwrap_or(Binding {
        session_id: session_id.into(),
        user_id: config.user_id.clone(),
        organization_id: config.organization_id.clone(),
        api_base_url: config.api_base_url.clone(),
        admission: Value::Null,
        observed_at_ms,
        platform: platform.into(),
        call_identity: None,
        verification_pending: false,
        connection_id: None,
        quarantine_reason: None,
    });
    if binding.user_id != config.user_id
        || binding.organization_id != config.organization_id
        || binding.api_base_url != config.api_base_url
    {
        return Err("Capture belongs to another sharing account".into());
    }
    binding.quarantine(reason);
    binding.verification_pending = false;
    save(&path, &binding)
}
fn quarantine_configuration_change(
    dir: &Path,
    state: &QueueState,
    config: &Configuration,
    changed_at_ms: i64,
) -> Result<(), String> {
    if let Some(previous) = read::<Configuration>(&dir.join("configuration.json"))? {
        let connections = |config: &Configuration| {
            config
                .connections
                .iter()
                .map(|connection| {
                    (
                        connection.integration_id.clone(),
                        connection.connection_id.clone(),
                        connection.status.clone(),
                    )
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        if previous.enabled
            && (!config.enabled
                || previous.user_id != config.user_id
                || previous.organization_id != config.organization_id
                || previous.api_base_url != config.api_base_url
                || connections(&previous) != connections(config))
        {
            let captures = state
                .raw
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .filter(|(_, timeline)| {
                    timeline
                        .stopped_at_ms
                        .is_none_or(|stopped| stopped >= changed_at_ms)
                        && timeline.owned_by(&previous)
                })
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>();
            for session_id in captures {
                quarantine_binding(
                    dir,
                    &previous,
                    &session_id,
                    "unverified",
                    changed_at_ms,
                    "configuration_changed",
                )?;
            }
        }
    }
    Ok(())
}
fn quarantine_unadmitted(
    dir: &Path,
    state: &QueueState,
    config: &Configuration,
    session_id: &str,
    platform: &str,
    observed_at_ms: i64,
) -> Result<(), String> {
    if state
        .raw
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(session_id)
        .is_some_and(|timeline| timeline.owned_by(config))
    {
        quarantine_binding(
            dir,
            config,
            session_id,
            platform,
            observed_at_ms,
            "initial_admission_failed",
        )?;
    }
    Ok(())
}

pub(crate) fn clear_legacy_playback<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Ok(base) = app.path().app_data_dir() else {
        return;
    };
    clear_legacy_playback_at(&base);
}
fn clear_legacy_playback_at(base: &Path) {
    let outbox = base.join("shared-recording-outbox");
    let playback = outbox.join("playback");
    if [base, outbox.as_path(), playback.as_path()]
        .iter()
        .any(|path| {
            !path
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_dir())
        })
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(playback) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let known_name = path.extension().is_some_and(|extension| extension == "wav")
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    stem.len() == 64
                        && stem
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                });
        if known_name && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = std::fs::remove_file(path);
        }
    }
}
#[tauri::command]
#[specta::specta]
pub async fn shared_recording_configuration<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Option<Configuration>, String> {
    read(&directory(&app)?.join("configuration.json"))
}
#[tauri::command]
#[specta::specta]
pub async fn shared_recording_configure<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    config: Configuration,
) -> Result<Configuration, String> {
    let changed_at_ms = chrono::Utc::now().timestamp_millis();
    validate_url(&config.api_base_url)?;
    if config.organization_id.is_empty() || config.user_id.is_empty() {
        return Err("Choose an organization and signed-in account".into());
    }
    let queue_state = app.state::<QueueState>();
    let dir = directory(&app)?;
    {
        let _binding = queue_state
            .bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        quarantine_configuration_change(&dir, &queue_state, &config, changed_at_ms)?;
    }
    let _guard = queue_state.serial.lock().await;
    let _binding = queue_state
        .bindings
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    quarantine_configuration_change(&dir, &queue_state, &config, changed_at_ms)?;
    save(&dir.join("configuration.json"), &config)?;
    Ok(config)
}
#[tauri::command]
#[specta::specta]
pub async fn shared_recording_observe<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session_id: String,
    platform: String,
    call_identity: Option<String>,
    observed_at_ms: i64,
    access_token: String,
    user_id: String,
    quarantine_reason: Option<String>,
    diagnostic_summary: Option<ObservationSummary>,
) -> Result<Option<String>, String> {
    if platform == "unverified" && quarantine_reason.as_deref() == Some("user_excluded") {
        shared_recording_exclude(app, session_id, user_id).await?;
        return Ok(None);
    }
    ensure_allowed(&app, &session_id)?;
    let queue_state = app.state::<QueueState>();
    let dir = directory(&app)?;
    if platform == "unverified" {
        let _binding = queue_state
            .bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let config: Configuration =
            read(&dir.join("configuration.json"))?.ok_or("Shared recording is not configured")?;
        bound(&config, &user_id)?;
        let active_owner = queue_state
            .raw
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&session_id)
            .is_some_and(|timeline| {
                timeline.owned_by(&config)
                    && observed_at_ms >= timeline.started_at_ms
                    && timeline
                        .stopped_at_ms
                        .is_none_or(|stopped| observed_at_ms <= stopped)
            });
        if !active_owner {
            return Err("Capture is no longer active under this sharing account".into());
        }
        let reason = match quarantine_reason.as_deref() {
            Some(
                reason @ ("ax_unavailable" | "ax_ambiguous" | "meeting_unverified"
                | "inventory_missing" | "inventory_conflict" | "meeting_departed"
                | "auth_unavailable" | "observer_stopped" | "call_changed"),
            ) => reason,
            _ => "unverified",
        };
        if let Some(summary) = diagnostic_summary {
            let status = match summary.inspection_status.as_str() {
                "ok" => "ok",
                "error" => "error",
                _ => "unavailable",
            };
            let categories: Vec<_> = summary
                .warning_categories
                .iter()
                .filter_map(|category| match category.as_str() {
                    category @ ("ambiguous" | "incomplete" | "timeout" | "permission"
                    | "failed" | "unscoped" | "missing") => Some(category),
                    _ => None,
                })
                .take(7)
                .collect();
            tracing::warn!(reason, session = key(&session_id), inspection_status = status, row_count = summary.row_count.min(4096), active_call_count = summary.active_call_count.min(4096), trusted_count = summary.trusted_count.min(4096), warning_categories = ?categories, "shared_recording_observation_unverified");
        }
        quarantine_binding(
            &dir,
            &config,
            &session_id,
            &platform,
            observed_at_ms,
            reason,
        )?;
        return Ok(None);
    }
    let _observation = queue_state.observe_serial.lock().await;
    let provider = match platform.as_str() {
        "googleMeet" => "google_meet",
        "slack" => "slack_huddle",
        _ => return Err("Unsupported meeting provider".into()),
    };
    let integration = if platform == "googleMeet" {
        "google-meet"
    } else {
        "slack"
    };
    let path = dir.join(format!("binding-{}.json", key(&session_id)));
    let (config, connection_id, first_admission) = {
        let _preparation = queue_state.serial.lock().await;
        let _binding = queue_state
            .bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let config: Configuration =
            read(&dir.join("configuration.json"))?.ok_or("Shared recording is not configured")?;
        bound(&config, &user_id)?;
        let existing = read::<Binding>(&path)?;
        if let Some(mut binding) = existing.clone() {
            if binding.user_id != user_id
                || binding.organization_id != config.organization_id
                || binding.api_base_url != config.api_base_url
            {
                return Err("Capture is already bound to a different account".into());
            }
            if binding.platform != platform
                || (binding.call_identity.is_some()
                    && call_identity.is_some()
                    && binding.call_identity != call_identity)
            {
                binding.quarantine("call_changed");
                save(&path, &binding)?;
                return Err(
                    "Meeting changed during this recording; start a new recording to share it"
                        .into(),
                );
            }
            if binding.admission.is_null() {
                return Err(QUARANTINED_MESSAGE.into());
            }
            binding.verification_pending = true;
            save(&path, &binding)?;
        }
        let first_admission = existing.is_none();
        let Some(connection) = config.unique_connection(integration) else {
            if first_admission {
                quarantine_unadmitted(
                    &dir,
                    &queue_state,
                    &config,
                    &session_id,
                    &platform,
                    observed_at_ms,
                )?;
            }
            return Err("Connect exactly one account for this meeting provider".into());
        };
        if let Some(binding) = existing.as_ref() {
            if binding
                .connection_id
                .as_ref()
                .is_some_and(|id| id != &connection.connection_id)
            {
                return Err("Meeting connection changed; original stays local".into());
            }
            if binding.call_identity.is_some() && call_identity.is_none() {
                return Err("Meeting identity is unavailable; verification will retry".into());
            }
        }
        if access_token.is_empty() {
            if first_admission {
                quarantine_unadmitted(
                    &dir,
                    &queue_state,
                    &config,
                    &session_id,
                    &platform,
                    observed_at_ms,
                )?;
            }
            return Err("Sign in again to verify this meeting; original audio stays local".into());
        }
        let connection_id = connection.connection_id.clone();
        (config, connection_id, first_admission)
    };
    validate_url(&config.api_base_url)?;
    let url = format!(
        "{}/v1/orgs/{}/shared-recordings/admit",
        config.api_base_url.trim_end_matches('/'),
        config.organization_id
    );
    let request_started_at_ms = chrono::Utc::now().timestamp_millis();
    let cancel = cancellation(&queue_state, &session_id);
    let resolved = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(EXCLUDED_MESSAGE.into()),
        result = async {
        json_body(success(client()?.post(url).bearer_auth(access_token).json(&json!({"provider":provider,"roomIdentity":call_identity,"connectionId":connection_id})).send().await.map_err(|e|e.to_string())?).await?).await
        } => result,
    };
    let resolved = resolved.and_then(|admission| {
        if admission
            .get("occurrenceId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
        {
            Ok(admission)
        } else {
            Err("Provider did not resolve a meeting occurrence".into())
        }
    });
    let mut admission = match resolved {
        Ok(admission) => admission,
        Err(error) => {
            let _binding = queue_state
                .bindings
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if first_admission {
                quarantine_unadmitted(
                    &dir,
                    &queue_state,
                    &config,
                    &session_id,
                    &platform,
                    observed_at_ms,
                )?;
            }
            return Err(error);
        }
    };
    let response_at_ms = chrono::Utc::now().timestamp_millis();
    admission["localClockReferenceMs"] = json!(
        request_started_at_ms
            .saturating_add(response_at_ms.saturating_sub(request_started_at_ms) / 2)
    );
    admission["clockUncertaintyMs"] =
        json!(response_at_ms.saturating_sub(request_started_at_ms).max(0) / 2);
    let _guard = queue_state.serial.lock().await;
    let _binding = queue_state
        .bindings
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let current: Configuration =
        read(&dir.join("configuration.json"))?.ok_or("Shared recording disabled")?;
    bound(&current, &user_id)?;
    if current.organization_id != config.organization_id
        || current.api_base_url != config.api_base_url
        || current
            .unique_connection(integration)
            .map(|connection| &connection.connection_id)
            != Some(&connection_id)
    {
        return Err("Account configuration changed during admission".into());
    }
    ensure_allowed(&app, &session_id)?;
    let existing = read::<Binding>(&path)?;
    if let Some(mut binding) = existing {
        if binding.admission.is_null() {
            return Err(
                "Meeting activity became unverified during admission; original stays local".into(),
            );
        }
        if binding.admission["occurrenceId"] != admission["occurrenceId"] {
            binding.quarantine("occurrence_changed");
            save(&path, &binding)?;
            return Err(
                "Provider occurrence changed during this recording; original stays local".into(),
            );
        }
        binding.verification_pending = false;
        binding.connection_id = Some(connection_id);
        binding.call_identity = call_identity;
        save(&path, &binding)?;
        return Ok(binding.admission["occurrenceId"]
            .as_str()
            .map(str::to_owned));
    }
    let occurrence_id = admission["occurrenceId"].as_str().map(str::to_owned);
    save(
        &path,
        &Binding {
            session_id,
            user_id,
            organization_id: config.organization_id,
            api_base_url: config.api_base_url,
            admission,
            observed_at_ms,
            platform,
            call_identity,
            verification_pending: false,
            connection_id: Some(connection_id),
            quarantine_reason: None,
        },
    )?;
    Ok(occurrence_id)
}

fn normalize(source: &Path, dir: &Path) -> Result<Vec<Segment>, String> {
    let source = anlg_audio_utils::source_from_path(source).map_err(|e| e.to_string())?;
    let mut samples = Vec::with_capacity(16000 * 300);
    let mut segments = Vec::new();
    let mut emit = |samples: &mut Vec<i16>| -> Result<(), anlg_audio_utils::Error> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(
                &mut bytes,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )?;
            for sample in samples.iter() {
                writer.write_sample(*sample)?;
            }
            writer.finalize()?;
        }
        let bytes = bytes.into_inner();
        let sha256 = digest(&bytes);
        write_private(&dir.join(format!("{}.wav", segments.len())), &bytes)
            .map_err(std::io::Error::other)?;
        segments.push(Segment {
            sha256,
            frames: samples.len() as u64,
        });
        samples.clear();
        Ok(())
    };
    anlg_audio_utils::for_each_resampled_channel_block::<_, anlg_audio_utils::Error>(
        source,
        16000,
        |channels| {
            for index in 0..channels[0].len() {
                let sample = channels.iter().map(|c| c[index]).sum::<f32>() / channels.len() as f32;
                samples.push((sample * 32768.).clamp(-32768., 32767.) as i16);
                if samples.len() == 16000 * 300 {
                    emit(&mut samples)?;
                }
            }
            Ok(())
        },
    )
    .map_err(|e| e.to_string())?;
    emit(&mut samples).map_err(|e| e.to_string())?;
    if segments.is_empty() {
        return Err("Capture contains no decodable audio".into());
    }
    Ok(segments)
}

#[tauri::command]
#[specta::specta]
pub async fn shared_recording_enqueue<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    session_id: String,
    transcript_id: String,
    audio_path: String,
    started_at_ms: i64,
    stopped_at_ms: i64,
) -> Result<Option<Job>, String> {
    ensure_allowed(&app, &session_id)?;
    let _audio = app.fs_sync().lock_session_audio(&session_id).await;
    ensure_allowed(&app, &session_id)?;
    let dir = directory(&app)?;
    let Some(config) = read::<Configuration>(&dir.join("configuration.json"))? else {
        return Ok(None);
    };
    if !config.enabled {
        return Ok(None);
    }
    let queue_state = app.state::<QueueState>();
    let _guard = queue_state.serial.lock().await;
    let capture_owner = queue_state
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&session_id)
        .and_then(|t| t.owner.clone());
    let owner =
        capture_owner.ok_or("Capture sharing was not enabled when this recording started")?;
    if owner.user_id != config.user_id
        || owner.organization_id != config.organization_id
        || owner.api_base_url != config.api_base_url
    {
        return Err("Capture started under a different sharing account".into());
    }
    let device_id = match read::<String>(&dir.join("device-id.json"))? {
        Some(id) => id,
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            save(&dir.join("device-id.json"), &id)?;
            id
        }
    };
    let id = key(&format!(
        "{}:{}:{}:{}:{}",
        config.user_id, config.organization_id, device_id, session_id, transcript_id
    ));
    let job_dir = dir.join(&id);
    if let Some(job) = read::<Job>(&job_path(&dir, &id))? {
        return Ok(Some(job));
    }
    let base = app.settings().vault_base().map_err(|e| e.to_string())?;
    let session = tauri_plugin_fs_sync::find_session_dir(
        &base.join("sessions").into_std_path_buf(),
        &session_id,
    )
    .map_err(|e| e.to_string())?
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let path = PathBuf::from(audio_path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.starts_with(&session) {
        return Err("Only this session's captured audio can be shared".into());
    }
    private_directory(&job_dir)?;
    copy_private(&path, &job_dir.join("original.audio"))?;
    let observed_binding =
        read::<Binding>(&dir.join(format!("binding-{}.json", key(&session_id))))?.filter(|b| {
            b.user_id == config.user_id
                && b.organization_id == config.organization_id
                && b.api_base_url == config.api_base_url
        });
    let identity_quarantined = observed_binding
        .as_ref()
        .is_some_and(|binding| binding.admission.is_null());
    let binding = observed_binding.and_then(|b| b.valid_binding().then_some(b.admission));
    let (gaps, storage_failed, raw_frames) = {
        let raw = queue_state.raw.lock().unwrap_or_else(|e| e.into_inner());
        let timeline = raw.get(&session_id);
        (
            timeline.map(|t| t.gaps.clone()).unwrap_or_default(),
            timeline.is_some_and(|t| t.storage_failed),
            timeline.map(|t| t.frames).filter(|frames| *frames > 0),
        )
    };
    let mut job = Job {
        id,
        session_id,
        state: if storage_failed {
            "blocked_storage"
        } else if identity_quarantined {
            "blocked_identity"
        } else {
            "preparing"
        }
        .into(),
        error: if storage_failed {
            Some("Audio storage reported a failure. Original retained; sharing is blocked until its timeline can be verified.".into())
        } else {
            identity_quarantined.then(|| QUARANTINED_MESSAGE.into())
        },
        user_id: config.user_id,
        organization_id: config.organization_id,
        device_id,
        api_base_url: config.api_base_url,
        transcript_id,
        started_at_ms,
        stopped_at_ms,
        binding,
        segments: Vec::new(),
        gaps,
        raw_frames,
    };
    save(&job_path(&dir, &job.id), &job)?;
    drop(_audio);
    if storage_failed || identity_quarantined {
        return Ok(Some(job));
    }
    let target = job_dir.clone();
    job.segments =
        tokio::task::spawn_blocking(move || normalize(&target.join("original.audio"), &target))
            .await
            .map_err(|e| e.to_string())??;
    ensure_allowed(&app, &job.session_id)?;
    finish_preparation(&mut job);
    save(&job_path(&dir, &job.id), &job)?;
    Ok(Some(job))
}
#[tauri::command]
#[specta::specta]
pub async fn shared_recording_queue<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    user_id: String,
) -> Result<Vec<Job>, String> {
    let dir = directory(&app)?;
    if read::<Configuration>(&dir.join("configuration.json"))?.is_none_or(|c| c.user_id != user_id)
    {
        return Ok(Vec::new());
    }
    let mut jobs = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            if let Some(job) = read::<Job>(&entry.path().join("job.json"))? {
                if job.user_id == user_id {
                    jobs.push(job)
                }
            }
        }
    }
    Ok(jobs)
}
#[tauri::command]
#[specta::specta]
pub async fn shared_recording_flush<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    access_token: String,
    user_id: String,
) -> Result<(), String> {
    let config = shared_recording_configuration(app.clone())
        .await?
        .ok_or("Shared recording is not configured")?;
    bound(&config, &user_id)?;
    validate_url(&config.api_base_url)?;
    let queue_state = app.state::<QueueState>();
    let _guard = queue_state.flush_serial.lock().await;
    let dir = directory(&app)?;
    let jobs = shared_recording_queue(app.clone(), user_id.clone()).await?;
    let mut pending_attempts = HashSet::new();
    for mut job in jobs {
        if ensure_allowed(&app, &job.session_id).is_err() {
            continue;
        }
        if queue_state
            .pins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&job.session_id)
        {
            continue;
        }
        if job.state == "uploaded" {
            cleanup_uploaded_audio(&dir, &job)?;
            continue;
        }
        if job.state == "blocked_storage"
            || job.state == "blocked_timeline"
            || job.state == "blocked_identity"
            || job.organization_id != config.organization_id
            || job.api_base_url != config.api_base_url
        {
            continue;
        }
        let binding_path = dir.join(format!("binding-{}.json", key(&job.session_id)));
        if read::<Binding>(&binding_path)?.is_some_and(|binding| {
            binding.admission.is_null()
                && binding.user_id == user_id
                && binding.organization_id == config.organization_id
                && binding.api_base_url == config.api_base_url
        }) {
            job.state = "blocked_identity".into();
            job.binding = None;
            job.error = Some(QUARANTINED_MESSAGE.into());
            save(&job_path(&dir, &job.id), &job)?;
            continue;
        }
        if job.segments.is_empty() {
            let target = dir.join(&job.id);
            match tokio::task::spawn_blocking(move || {
                normalize(&target.join("original.audio"), &target)
            })
            .await
            .map_err(|e| e.to_string())?
            {
                Ok(segments) => {
                    job.segments = segments;
                    finish_preparation(&mut job);
                    save(&job_path(&dir, &job.id), &job)?;
                    if job.state == "blocked_timeline" {
                        continue;
                    }
                }
                Err(error) => {
                    job.state = "error".into();
                    job.error = Some(error);
                    save(&job_path(&dir, &job.id), &job)?;
                    continue;
                }
            }
        }
        if let Some(binding) = read::<Binding>(&binding_path)?.filter(|binding| {
            binding.verification_pending
                && !binding.admission.is_null()
                && binding.user_id == user_id
                && binding.organization_id == config.organization_id
                && binding.api_base_url == config.api_base_url
        }) {
            if !pending_attempts.insert(job.session_id.clone()) {
                continue;
            }
            let integration = match binding.platform.as_str() {
                "googleMeet" => "google-meet",
                "slack" => "slack",
                _ => continue,
            };
            let Some(connection) = config.unique_connection(integration) else {
                continue;
            };
            if binding.connection_id.as_deref() != Some(connection.connection_id.as_str()) {
                continue;
            }
            if shared_recording_observe(
                app.clone(),
                job.session_id.clone(),
                binding.platform,
                binding.call_identity,
                chrono::Utc::now().timestamp_millis(),
                access_token.clone(),
                user_id.clone(),
                None,
                None,
            )
            .await
            .is_err()
            {
                continue;
            }
        }
        if job.binding.is_none() {
            job.binding = read::<Binding>(&binding_path)?
                .filter(|b| {
                    b.user_id == user_id
                        && b.organization_id == config.organization_id
                        && b.api_base_url == config.api_base_url
                })
                .and_then(|b| b.valid_binding().then_some(b.admission));
        }
        if job.binding.is_none() {
            continue;
        }
        match upload(&dir, &job, &access_token, &queue_state).await {
            Ok(()) => {
                job.state = "uploaded".into();
                job.error = None;
            }
            Err(error) => {
                job.state = "error".into();
                job.error = Some(error);
            }
        }
        save(&job_path(&dir, &job.id), &job)?;
        if job.state == "uploaded" {
            cleanup_uploaded_audio(&dir, &job)?;
        }
    }
    Ok(())
}
fn verify_upload_binding(dir: &Path, job: &Job, state: &QueueState) -> Result<(), String> {
    let _binding = state
        .bindings
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if exclusion_path(dir, &job.session_id)
        .try_exists()
        .map_err(|e| e.to_string())?
        || state
            .excluded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&job.session_id)
    {
        return Err(EXCLUDED_MESSAGE.into());
    }
    let latest = read::<Binding>(&dir.join(format!("binding-{}.json", key(&job.session_id))))?
        .ok_or("Meeting admission is unavailable; original retained")?;
    let admission = job.binding.as_ref().ok_or("Meeting identity unresolved")?;
    if !latest.valid_binding()
        || latest.user_id != job.user_id
        || latest.organization_id != job.organization_id
        || latest.api_base_url != job.api_base_url
        || latest.admission["occurrenceId"] != admission["occurrenceId"]
    {
        return Err("Meeting admission changed; original retained".into());
    }
    let config: Configuration = read(&dir.join("configuration.json"))?.ok_or("Sharing disabled")?;
    bound(&config, &job.user_id)?;
    if config.organization_id != job.organization_id || config.api_base_url != job.api_base_url {
        return Err("Sharing account changed".into());
    }
    if let Some(connection_id) = latest.connection_id.as_ref() {
        let integration = match latest.platform.as_str() {
            "googleMeet" => "google-meet",
            "slack" => "slack",
            _ => return Err("Meeting provider changed; original retained".into()),
        };
        if config
            .unique_connection(integration)
            .map(|connection| &connection.connection_id)
            != Some(connection_id)
        {
            return Err("Meeting connection changed; original retained".into());
        }
    }
    Ok(())
}
async fn upload(dir: &Path, job: &Job, token: &str, state: &QueueState) -> Result<(), String> {
    let cancel = cancellation(state, &job.session_id);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(EXCLUDED_MESSAGE.into()),
        result = upload_allowed(dir, job, token, state) => result,
    }
}
async fn upload_allowed(
    dir: &Path,
    job: &Job,
    token: &str,
    state: &QueueState,
) -> Result<(), String> {
    let binding = job.binding.as_ref().ok_or("Meeting identity unresolved")?;
    let occurrence = binding["occurrenceId"]
        .as_str()
        .ok_or("Meeting identity unresolved")?;
    let base = format!(
        "{}/v1/orgs/{}/shared-recordings/{}",
        job.api_base_url.trim_end_matches('/'),
        job.organization_id,
        occurrence
    );
    let mut segment_start = 0u64;
    let server_reference = binding["serverObservedAtMs"]
        .as_i64()
        .unwrap_or(job.started_at_ms);
    let clock_reference = binding["localClockReferenceMs"]
        .as_i64()
        .unwrap_or(server_reference);
    let provider_started = binding["providerStartedAtMs"]
        .as_i64()
        .unwrap_or(server_reference);
    let mut offset = job
        .started_at_ms
        .saturating_add(server_reference.saturating_sub(clock_reference))
        .saturating_sub(provider_started)
        .saturating_mul(16);
    for (sequence, segment) in job.segments.iter().enumerate() {
        verify_upload_binding(dir, job, state)?;
        let bytes = std::fs::read(dir.join(&job.id).join(format!("{sequence}.wav")))
            .map_err(|e| e.to_string())?;
        if digest(&bytes) != segment.sha256 {
            return Err("Queued audio checksum mismatch".into());
        }
        let gaps: Vec<Value> = job
            .gaps
            .iter()
            .filter_map(|gap| {
                let start = gap.start.max(segment_start);
                let end = gap.end.min(segment_start + segment.frames);
                (start < end).then(|| json!({"start":start-segment_start,"end":end-segment_start}))
            })
            .collect();
        let metadata = json!({"uploaderId":job.user_id,"deviceId":job.device_id,"localCaptureId":job.id,"segmentSequence":sequence,"localSessionId":job.session_id,"localTranscriptId":job.transcript_id,"sha256":segment.sha256,"offsetFrames":offset,"rank":0,"gaps":gaps,"timingBasis":"device_clock_hint"});
        let request = client()?.post(format!("{base}/captures/{}:{sequence}",job.id)).bearer_auth(token).json(&json!({"metadata":metadata,"wavBase64":base64::engine::general_purpose::STANDARD.encode(bytes)}));
        verify_upload_binding(dir, job, state)?;
        success(request.send().await.map_err(|e| e.to_string())?).await?;
        offset += segment.frames as i64;
        segment_start += segment.frames;
    }
    verify_upload_binding(dir, job, state)?;
    success(
        client()?
            .post(format!("{base}/process"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?;
    Ok(())
}
pub(crate) fn snapshot_stopped<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session_id: &str,
    audio_path: Option<&str>,
    started_at_ms: Option<i64>,
    stopped_at_ms: i64,
) {
    let state = app.state::<QueueState>();
    let capture_start = {
        let mut raw = state.raw.lock().unwrap_or_else(|error| error.into_inner());
        let Some(timeline) = raw.get_mut(session_id) else {
            return;
        };
        timeline.stopped_at_ms = Some(stopped_at_ms);
        timeline.owner.as_ref().map(|_| timeline.started_at_ms)
    };
    if ensure_allowed(app, session_id).is_err() {
        return;
    }
    let Some(audio_path) = audio_path.map(str::to_string) else {
        return;
    };
    let Some(capture_start) = capture_start else {
        return;
    };
    state
        .pins
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session_id.to_string());
    let started_at_ms = Some(capture_start).filter(|ms| *ms > 0).or(started_at_ms);
    let app = app.clone();
    let session_id = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        let marker = crate::capture_markers::get_marker(&app, &session_id)
            .await
            .ok()
            .flatten();
        let transcript_id = marker
            .as_ref()
            .map(|m| m.transcript_id.clone())
            .unwrap_or_else(|| session_id.clone());
        let result = shared_recording_enqueue(
            app.clone(),
            session_id.clone(),
            transcript_id,
            audio_path,
            started_at_ms.unwrap_or(stopped_at_ms),
            stopped_at_ms,
        )
        .await;
        if let Err(error) = result {
            tracing::warn!(%error,%session_id,"shared_recording_snapshot_failed");
        }
        app.state::<QueueState>()
            .pins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session_id);
    });
}

#[cfg(test)]
mod tests;
