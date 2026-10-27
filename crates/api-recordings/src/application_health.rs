use crate::{recording_app::RecordingAppState, recording_members::MemberError};
use anlg_api_auth::AuthContext;
use axum::{Extension, Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    device_id: String,
    app_version: String,
    observed_at: DateTime<Utc>,
    permissions: Permissions,
    auto_record_detected: bool,
    auto_record_scheduled: bool,
    recording: Option<bool>,
    upload_pending_count: Option<u32>,
    upload_blocked_count: Option<u32>,
    upload_error: Option<bool>,
    last_recording_started_at: Option<DateTime<Utc>>,
    last_recording_stopped_at: Option<DateTime<Utc>>,
    last_recording_duration_seconds: Option<u32>,
    error_category: Option<ErrorCategory>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Permissions {
    microphone: Permission,
    system_audio: Permission,
    accessibility: Permission,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum Permission {
    Authorized,
    Denied,
    NeverRequested,
    Unknown,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCategory {
    AudioCapture,
    Transcription,
}

pub async fn observe(
    State(state): State<RecordingAppState>,
    Extension(auth): Extension<AuthContext>,
    Json(snapshot): Json<Snapshot>,
) -> Result<StatusCode, MemberError> {
    let now = Utc::now();
    let valid_uuid = snapshot.device_id.len() == 36
        && snapshot.device_id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        });
    if snapshot.upload_pending_count.is_some_and(|v| v > 4096)
        || snapshot.upload_blocked_count.is_some_and(|v| v > 4096)
        || !valid_uuid
        || snapshot.app_version.is_empty()
        || snapshot.app_version.len() > 64
        || !snapshot
            .app_version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+_-".contains(&b))
        || snapshot.observed_at > now + chrono::Duration::minutes(5)
        || snapshot.observed_at < now - chrono::Duration::hours(24)
        || snapshot
            .last_recording_duration_seconds
            .is_some_and(|v| v > 86400)
        || snapshot
            .last_recording_started_at
            .is_some_and(|v| v > now + chrono::Duration::minutes(5))
        || snapshot
            .last_recording_stopped_at
            .is_some_and(|v| v > now + chrono::Duration::minutes(5))
    {
        return Err(MemberError::Invalid("invalid health snapshot".into()));
    }
    if state.members.memberships(&auth).await?.is_empty() {
        return Err(MemberError::Forbidden);
    }
    let snapshot = serde_json::to_value(snapshot).map_err(|_| MemberError::Upstream)?;
    state.members.observe_health(&auth, snapshot).await?;
    Ok(StatusCode::NO_CONTENT)
}
