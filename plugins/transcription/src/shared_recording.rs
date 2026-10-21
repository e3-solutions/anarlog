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
}
impl Binding {
    fn valid_binding(&self) -> bool {
        !self.verification_pending && !self.admission.is_null()
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
    started_at_ms: i64,
    storage_failed: bool,
}
#[derive(Default)]
pub(crate) struct QueueState {
    pub(crate) pins: Mutex<HashSet<String>>,
    serial: tokio::sync::Mutex<()>,
    observe_serial: tokio::sync::Mutex<()>,
    flush_serial: tokio::sync::Mutex<()>,
    raw: Mutex<HashMap<String, RawTimeline>>,
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
                started_at_ms: chrono::Utc::now().timestamp_millis(),
                ..Default::default()
            },
        );
    shared
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
    let tmp = path.with_extension("pending");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}
fn copy_private(source: &Path, target: &Path) -> Result<(), String> {
    let pending = target.with_extension("pending");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut input = std::fs::File::open(source).map_err(|e| e.to_string())?;
    let mut output = options.open(&pending).map_err(|e| e.to_string())?;
    std::io::copy(&mut input, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(pending, target).map_err(|e| e.to_string())
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

pub(crate) fn prune_playback<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Ok(base) = app.path().app_data_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(base.join("shared-recording-outbox/playback")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "wav") {
            expire_playback(&path);
        }
    }
}
fn expire_playback(path: &Path) {
    if path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= std::time::Duration::from_secs(86400))
    {
        let _ = std::fs::remove_file(path);
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
    validate_url(&config.api_base_url)?;
    if config.organization_id.is_empty() || config.user_id.is_empty() {
        return Err("Choose an organization and signed-in account".into());
    }
    let queue_state = app.state::<QueueState>();
    let _guard = queue_state.serial.lock().await;
    save(&directory(&app)?.join("configuration.json"), &config)?;
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
) -> Result<(), String> {
    let queue_state = app.state::<QueueState>();
    let _observation = queue_state.observe_serial.lock().await;
    let dir = directory(&app)?;
    let config: Configuration =
        read(&dir.join("configuration.json"))?.ok_or("Shared recording is not configured")?;
    bound(&config, &user_id)?;
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
    let connections: Vec<_> = config
        .connections
        .iter()
        .filter(|c| c.status == "connected" && c.integration_id == integration)
        .collect();
    if connections.len() != 1 {
        return Err("Connect exactly one account for this meeting provider".into());
    }
    let connection = connections[0];
    let path = dir.join(format!("binding-{}.json", key(&session_id)));
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
            binding.admission = Value::Null;
            save(&path, &binding)?;
            return Err(
                "Meeting changed during this recording; start a new recording to share it".into(),
            );
        }
        if binding.admission.is_null() {
            return Err(
                "This recording spans different meetings and cannot be shared automatically".into(),
            );
        }
        binding.verification_pending = true;
        save(&path, &binding)?;
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
    validate_url(&config.api_base_url)?;
    let url = format!(
        "{}/v1/orgs/{}/shared-recordings/admit",
        config.api_base_url.trim_end_matches('/'),
        config.organization_id
    );
    let request_started_at_ms = chrono::Utc::now().timestamp_millis();
    let resolved = async {
        json_body(success(client()?.post(url).bearer_auth(access_token).json(&json!({"provider":provider,"roomIdentity":call_identity,"connectionId":connection.connection_id})).send().await.map_err(|e|e.to_string())?).await?).await
    }.await;
    let mut admission = resolved?;
    let response_at_ms = chrono::Utc::now().timestamp_millis();
    admission["localClockReferenceMs"] = json!(
        request_started_at_ms
            .saturating_add(response_at_ms.saturating_sub(request_started_at_ms) / 2)
    );
    admission["clockUncertaintyMs"] =
        json!(response_at_ms.saturating_sub(request_started_at_ms).max(0) / 2);
    if admission
        .get("occurrenceId")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err("Provider did not resolve a meeting occurrence".into());
    }
    let _guard = queue_state.serial.lock().await;
    let current: Configuration =
        read(&dir.join("configuration.json"))?.ok_or("Shared recording disabled")?;
    bound(&current, &user_id)?;
    let current_connections: Vec<_> = current
        .connections
        .iter()
        .filter(|c| c.status == "connected" && c.integration_id == integration)
        .collect();
    if current.organization_id != config.organization_id
        || current.api_base_url != config.api_base_url
        || current_connections.len() != 1
        || current_connections[0].connection_id != connection.connection_id
    {
        return Err("Account configuration changed during admission".into());
    }
    let connection_id = connection.connection_id.clone();
    if let Some(mut binding) = existing {
        if binding.admission["occurrenceId"] != admission["occurrenceId"] {
            binding.admission = Value::Null;
            save(&path, &binding)?;
            return Err(
                "Provider occurrence changed during this recording; original stays local".into(),
            );
        }
        binding.verification_pending = false;
        binding.connection_id = Some(connection_id);
        binding.call_identity = call_identity;
        save(&path, &binding)?;
        return Ok(());
    }
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
        },
    )
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
    let _audio = app.fs_sync().lock_session_audio(&session_id).await;
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
    let binding = read::<Binding>(&dir.join(format!("binding-{}.json", key(&session_id))))?
        .filter(|b| {
            b.user_id == config.user_id
                && b.organization_id == config.organization_id
                && b.api_base_url == config.api_base_url
        })
        .and_then(|b| b.valid_binding().then_some(b.admission));
    let gaps = app
        .state::<QueueState>()
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&session_id)
        .map(|t| t.gaps.clone())
        .unwrap_or_default();
    let storage_failed = queue_state
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&session_id)
        .is_some_and(|t| t.storage_failed);
    let raw_frames = queue_state
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&session_id)
        .map(|timeline| timeline.frames)
        .filter(|frames| *frames > 0);
    let mut job = Job {
        id,
        session_id,
        state: if storage_failed { "blocked_storage" } else { "preparing" }.into(),
        error: storage_failed.then(|| "Audio storage reported a failure. Original retained; sharing is blocked until its timeline can be verified.".into()),
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
    if storage_failed {
        return Ok(Some(job));
    }
    let target = job_dir.clone();
    job.segments =
        tokio::task::spawn_blocking(move || normalize(&target.join("original.audio"), &target))
            .await
            .map_err(|e| e.to_string())??;
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
            || job.organization_id != config.organization_id
            || job.api_base_url != config.api_base_url
        {
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
        let binding_path = dir.join(format!("binding-{}.json", key(&job.session_id)));
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
            let connections: Vec<_> = config
                .connections
                .iter()
                .filter(|connection| {
                    connection.status == "connected" && connection.integration_id == integration
                })
                .collect();
            if connections.len() != 1
                || binding.connection_id.as_deref() != Some(connections[0].connection_id.as_str())
            {
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
        match upload(&dir, &job, &access_token).await {
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
fn verify_upload_binding(dir: &Path, job: &Job) -> Result<(), String> {
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
        let connections: Vec<_> = config
            .connections
            .iter()
            .filter(|connection| {
                connection.status == "connected" && connection.integration_id == integration
            })
            .collect();
        if connections.len() != 1 || &connections[0].connection_id != connection_id {
            return Err("Meeting connection changed; original retained".into());
        }
    }
    Ok(())
}
async fn upload(dir: &Path, job: &Job, token: &str) -> Result<(), String> {
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
        verify_upload_binding(dir, job)?;
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
        success(client()?.post(format!("{base}/captures/{}:{sequence}",job.id)).bearer_auth(token).json(&json!({"metadata":metadata,"wavBase64":base64::engine::general_purpose::STANDARD.encode(bytes)})).send().await.map_err(|e|e.to_string())?).await?;
        offset += segment.frames as i64;
        segment_start += segment.frames;
    }
    verify_upload_binding(dir, job)?;
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
#[tauri::command]
#[specta::specta]
pub async fn shared_recording_download<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    api_base_url: String,
    organization_id: String,
    occurrence_id: String,
    revision: String,
    audio_sha256: String,
    access_token: String,
    user_id: String,
) -> Result<String, String> {
    validate_url(&api_base_url)?;
    let account = json_body(
        success(
            client()?
                .get(format!(
                    "{}/v1/shared-recordings/me",
                    api_base_url.trim_end_matches('/')
                ))
                .bearer_auth(&access_token)
                .send()
                .await
                .map_err(|e| e.to_string())?,
        )
        .await?,
    )
    .await?;
    if account["userId"].as_str() != Some(user_id.as_str()) {
        return Err("Account does not match shared recording".into());
    }
    let response = success(
        client()?
            .get(format!(
                "{}/v1/orgs/{}/shared-recordings/{}/revisions/{}/audio",
                api_base_url.trim_end_matches('/'),
                organization_id,
                occurrence_id,
                revision
            ))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| e.to_string())?,
    )
    .await?;
    let bytes = bounded_body(response, 512 * 1024 * 1024).await?;
    if digest(&bytes) != audio_sha256 {
        return Err("Shared audio checksum mismatch or download too large".into());
    }
    let dir = directory(&app)?.join("playback");
    private_directory(&dir)?;
    let path = dir.join(format!(
        "{}.wav",
        key(&format!(
            "{user_id}:{organization_id}:{occurrence_id}:{revision}"
        ))
    ));
    write_private(&path, &bytes)?;
    app.asset_protocol_scope()
        .allow_file(&path)
        .map_err(|error| error.to_string())?;
    let expiring = path.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(86400)).await;
        expire_playback(&expiring);
    });
    Ok(path.to_string_lossy().to_string())
}

pub(crate) fn snapshot_stopped(
    app: &tauri::AppHandle,
    session_id: &str,
    audio_path: Option<&str>,
    started_at_ms: Option<i64>,
    stopped_at_ms: i64,
) {
    let Some(audio_path) = audio_path.map(str::to_string) else {
        return;
    };
    let enabled = app
        .state::<QueueState>()
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .is_some_and(|t| t.owner.is_some());
    if !enabled {
        return;
    }
    app.state::<QueueState>()
        .pins
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session_id.to_string());
    let raw_started_at_ms = app
        .state::<QueueState>()
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .map(|t| t.started_at_ms)
        .filter(|ms| *ms > 0);
    let started_at_ms = raw_started_at_ms.or(started_at_ms);
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
mod tests {
    use super::*;

    fn job_fixture() -> Job {
        Job {
            id: "job".into(),
            session_id: "session".into(),
            state: "preparing".into(),
            error: None,
            user_id: "user".into(),
            organization_id: "org".into(),
            device_id: "device".into(),
            api_base_url: "http://localhost:8789".into(),
            transcript_id: "transcript".into(),
            started_at_ms: 0,
            stopped_at_ms: 1000,
            binding: None,
            segments: vec![Segment {
                sha256: "a".repeat(64),
                frames: 16100,
            }],
            gaps: vec![Gap {
                start: 8000,
                end: 9000,
            }],
            raw_frames: Some(16000),
        }
    }

    #[tokio::test]
    async fn temporary_admission_failure_blocks_uploads_until_the_same_meeting_is_reverified() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            for (status, body, expected) in [
                (200, r#"{"occurrenceId":"first"}"#, "/admit"),
                (503, "{}", "/admit"),
                (503, "{}", "/admit"),
                (200, r#"{"occurrenceId":"first"}"#, "/admit"),
                (200, "{}", "/captures/"),
                (200, "{}", "/process"),
                (200, "{}", "/captures/"),
                (200, "{}", "/process"),
                (200, r#"{"occurrenceId":"first"}"#, "/admit"),
                (200, r#"{"occurrenceId":"different"}"#, "/admit"),
            ] {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "missing request");
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let length = stream.read(&mut buffer).unwrap();
                    assert!(length > 0);
                    request.extend_from_slice(&buffer[..length]);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + content_length {
                            break;
                        }
                    }
                }
                assert!(
                    String::from_utf8_lossy(&request)
                        .lines()
                        .next()
                        .unwrap()
                        .contains(expected)
                );
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let root =
            std::env::temp_dir().join(format!("shared-revalidation-{}", uuid::Uuid::new_v4()));
        let session_id = uuid::Uuid::new_v4().to_string();
        let session_dir = root.join("sessions").join(&session_id);
        private_directory(&session_dir).unwrap();
        let audio = session_dir.join("audio.wav");
        let mut writer = hound::WavWriter::create(
            &audio,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..16000 {
            writer.write_sample(1000i16).unwrap();
        }
        writer.finalize().unwrap();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = format!(
            "ai.e3group.shared-revalidation-test-{}",
            uuid::Uuid::new_v4()
        );
        let app = tauri::test::mock_builder()
            .manage(tauri_plugin_fs_sync::SessionAudioLocks::default())
            .manage(tauri_plugin_settings::StartupSnapshot::new(root.clone()))
            .manage(QueueState::default())
            .build(context)
            .unwrap();
        let handle = app.handle().clone();
        shared_recording_configure(
            handle.clone(),
            Configuration {
                enabled: true,
                user_id: "user".into(),
                organization_id: "org".into(),
                api_base_url: format!("http://{address}"),
                connections: vec![Connection {
                    integration_id: "slack".into(),
                    connection_id: "connection".into(),
                    status: "connected".into(),
                }],
            },
        )
        .await
        .unwrap();
        assert!(start_raw(&handle, &session_id, Some("user")));
        observe_raw(&handle, &session_id, 16000, true);
        let observe = |call_identity| {
            shared_recording_observe(
                handle.clone(),
                session_id.clone(),
                "slack".into(),
                call_identity,
                0,
                "token".into(),
                "user".into(),
            )
        };
        observe(None).await.unwrap();
        let before = shared_recording_enqueue(
            handle.clone(),
            session_id.clone(),
            "before".into(),
            audio.to_string_lossy().into_owned(),
            0,
            1000,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(before.state, "pending");
        assert!(observe(Some("slack://room/first".into())).await.is_err());
        let path = directory(&handle)
            .unwrap()
            .join(format!("binding-{}.json", key(&session_id)));
        let held: Binding = read(&path).unwrap().unwrap();
        assert!(held.verification_pending);
        assert_eq!(held.admission["occurrenceId"], "first");
        let after = shared_recording_enqueue(
            handle.clone(),
            session_id.clone(),
            "after".into(),
            audio.to_string_lossy().into_owned(),
            0,
            1000,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(after.state, "awaiting_identity");
        assert!(after.binding.is_none());
        shared_recording_flush(handle.clone(), "token".into(), "user".into())
            .await
            .unwrap();
        assert!(
            shared_recording_queue(handle.clone(), "user".into())
                .await
                .unwrap()
                .iter()
                .all(|job| job.state != "uploaded")
        );
        shared_recording_flush(handle.clone(), "token".into(), "user".into())
            .await
            .unwrap();
        assert!(
            !read::<Binding>(&path)
                .unwrap()
                .unwrap()
                .verification_pending
        );
        assert!(
            shared_recording_queue(handle.clone(), "user".into())
                .await
                .unwrap()
                .iter()
                .all(|job| job.state == "uploaded")
        );
        observe(Some("slack://room/first".into())).await.unwrap();
        assert_eq!(
            read::<Binding>(&path)
                .unwrap()
                .unwrap()
                .call_identity
                .as_deref(),
            Some("slack://room/first")
        );
        assert!(observe(Some("slack://room/first".into())).await.is_err());
        assert!(read::<Binding>(&path).unwrap().unwrap().admission.is_null());
        assert!(observe(Some("slack://room/first".into())).await.is_err());
        server.join().unwrap();
        std::fs::remove_dir_all(directory(&handle).unwrap().parent().unwrap()).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn enqueue_uses_the_authenticated_capture_owner_without_a_local_note_owner() {
        let root = std::env::temp_dir().join(format!("shared-owner-{}", uuid::Uuid::new_v4()));
        let session_id = uuid::Uuid::new_v4().to_string();
        let session_dir = root.join("sessions").join(&session_id);
        private_directory(&session_dir).unwrap();
        let audio = session_dir.join("audio.wav");
        let mut writer = hound::WavWriter::create(
            &audio,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..16000 {
            writer.write_sample(1000i16).unwrap();
        }
        writer.finalize().unwrap();
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier =
            format!("ai.e3group.shared-owner-test-{}", uuid::Uuid::new_v4());
        let app = tauri::test::mock_builder()
            .manage(tauri_plugin_fs_sync::SessionAudioLocks::default())
            .manage(tauri_plugin_settings::StartupSnapshot::new(root.clone()))
            .manage(QueueState::default())
            .build(context)
            .unwrap();
        let handle = app.handle().clone();
        let config = Configuration {
            enabled: true,
            user_id: "auth-user".into(),
            organization_id: "org".into(),
            api_base_url: "http://localhost:8789".into(),
            connections: Vec::new(),
        };
        shared_recording_configure(handle.clone(), config.clone())
            .await
            .unwrap();
        assert!(
            shared_recording_enqueue(
                handle.clone(),
                session_id.clone(),
                "transcript".into(),
                audio.to_string_lossy().into_owned(),
                0,
                1000
            )
            .await
            .is_err()
        );
        assert!(start_raw(&handle, &session_id, Some("auth-user")));
        observe_raw(&handle, &session_id, 16000, true);
        let job = shared_recording_enqueue(
            handle.clone(),
            session_id.clone(),
            "transcript".into(),
            audio.to_string_lossy().into_owned(),
            0,
            1000,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(job.user_id, "auth-user");
        assert_eq!(job.state, "awaiting_identity");
        shared_recording_configure(
            handle.clone(),
            Configuration {
                user_id: "another-account".into(),
                ..config
            },
        )
        .await
        .unwrap();
        assert!(
            shared_recording_enqueue(
                handle.clone(),
                session_id,
                "transcript".into(),
                audio.to_string_lossy().into_owned(),
                0,
                1000
            )
            .await
            .err()
            .unwrap()
            .contains("different sharing account")
        );
        std::fs::remove_dir_all(directory(&handle).unwrap().parent().unwrap()).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codec_padding_keeps_failed_frames_uncovered_and_missing_frames_block_sharing() {
        let mut job = job_fixture();
        finish_preparation(&mut job);
        assert_eq!((job.gaps[0].start, job.gaps[0].end), (8000, 9100));
        job.raw_frames = Some(17000);
        finish_preparation(&mut job);
        assert_eq!(job.state, "blocked_timeline");
    }

    #[test]
    fn uploaded_retry_audio_is_removed_without_deleting_the_receipt() {
        let dir =
            std::env::temp_dir().join(format!("shared-outbox-cleanup-{}", uuid::Uuid::new_v4()));
        private_directory(&dir.join("job")).unwrap();
        let mut job = job_fixture();
        job.state = "uploaded".into();
        save(&job_path(&dir, &job.id), &job).unwrap();
        write_private(&dir.join("job/original.audio"), b"original").unwrap();
        write_private(&dir.join("job/0.wav"), b"segment").unwrap();
        cleanup_uploaded_audio(&dir, &job).unwrap();
        cleanup_uploaded_audio(&dir, &job).unwrap();
        assert!(!dir.join("job/original.audio").exists());
        assert!(!dir.join("job/0.wav").exists());
        assert_eq!(
            read::<Job>(&job_path(&dir, &job.id))
                .unwrap()
                .unwrap()
                .state,
            "uploaded"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn upload_snapshot_is_decodable_mono_and_keeps_captured_silence() {
        let dir = std::env::temp_dir().join(format!(
            "shared-recording-normalize-{}",
            uuid::Uuid::new_v4()
        ));
        private_directory(&dir).unwrap();
        let input = dir.join("original.wav");
        let output = dir.join("outbox");
        private_directory(&output).unwrap();
        let mut writer = hound::WavWriter::create(
            &input,
            hound::WavSpec {
                channels: 2,
                sample_rate: 48000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for frame in 0..48000 {
            let sample = if frame < 24000 { 8000i16 } else { 0 };
            writer.write_sample(sample).unwrap();
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let segments = normalize(&input, &output).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].frames, 16000);
        let mut reader = hound::WavReader::open(output.join("0.wav")).unwrap();
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().sample_rate, 16000);
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(samples[1000] > 7000);
        assert_eq!(samples[15000], 0);
        assert!(input.exists());
        let hash = segments[0].sha256.clone();
        assert_eq!(normalize(&input, &output).unwrap()[0].sha256, hash);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
