use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;

use crate::{
    recording_identity::{AdmissionRequest, RecordingIdentityResolver, ResolvedOccurrence},
    recording_members::{
        MemberError, OrganizationMembership, ProviderConnection, RecordingMemberAuth,
    },
    recording_transcription::RecordingProcessor,
    shared_recordings::{
        CaptureUpload, OccurrenceManifest, RecordingError, Revision, SharedRecordingService,
        TimingBasis,
    },
};

#[derive(Clone)]
pub struct RecordingAppState {
    pub service: Arc<SharedRecordingService>,
    pub members: RecordingMemberAuth,
    pub identities: Option<RecordingIdentityResolver>,
    pub processor: Option<Arc<RecordingProcessor>>,
}

pub fn router(state: RecordingAppState) -> Router {
    Router::new()
        .route("/v1/shared-recordings/me", get(me))
        .route("/v1/orgs/{workspace}/shared-recordings", get(library))
        .route("/v1/orgs/{workspace}/shared-recordings/admit", post(admit))
        .route(
            "/v1/orgs/{workspace}/shared-recordings/{id}",
            get(manifest).delete(delete_occurrence),
        )
        .route(
            "/v1/orgs/{workspace}/shared-recordings/{id}/audio",
            get(audio),
        )
        .route(
            "/v1/orgs/{workspace}/shared-recordings/{id}/revisions/{revision}/audio",
            get(revision_audio),
        )
        .route(
            "/v1/orgs/{workspace}/shared-recordings/{id}/transcript",
            get(transcript),
        )
        .route(
            "/v1/orgs/{workspace}/shared-recordings/{id}/captures/{source}",
            post(upload).delete(delete_capture),
        )
        .route(
            "/v1/orgs/{workspace}/shared-recordings/{id}/process",
            post(process),
        )
        .route(
            "/v1/orgs/{workspace}/shared-recordings/{id}/seal",
            post(seal),
        )
        .layer(DefaultBodyLimit::max(140 * 1024 * 1024))
        .layer(middleware::from_fn(no_store))
        .with_state(state)
}

async fn no_store(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    user_id: String,
    workspaces: Vec<OrganizationMembership>,
    connections: Vec<ProviderConnection>,
}
async fn me(
    State(state): State<RecordingAppState>,
    headers: HeaderMap,
) -> Result<Json<Account>, MemberError> {
    let auth = state.members.authenticate(&headers).await?;
    let workspaces = state.members.memberships(&auth).await?;
    let connections = state.members.connections(&auth).await?;
    Ok(Json(Account {
        user_id: auth.claims.sub,
        workspaces,
        connections,
    }))
}

#[derive(Serialize)]
struct Library {
    items: Vec<OccurrenceManifest>,
}
async fn library(
    State(state): State<RecordingAppState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Library>, MemberError> {
    state.members.authorize(&headers, &workspace).await?;
    Ok(Json(Library {
        items: state
            .service
            .list_occurrences(&workspace)
            .await
            .map_err(storage_error)?,
    }))
}

async fn admit(
    State(state): State<RecordingAppState>,
    Path(workspace): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AdmissionRequest>,
) -> Result<Json<ResolvedOccurrence>, MemberError> {
    let session = state.members.authorize(&headers, &workspace).await?;
    let resolver = state.identities.as_ref().ok_or(MemberError::Unresolved)?;
    let mut resolved = resolver.resolve(&session.auth, &request).await?;
    state
        .service
        .automatic_admit(
            &workspace,
            &resolved.occurrence_id,
            resolved.provider.clone(),
            &resolved.provider_occurrence_id,
            resolved.provider_started_at_ms,
        )
        .await
        .map_err(storage_error)?;
    resolved.provider_started_at_ms = state
        .service
        .occurrence_epoch(&workspace, &resolved.occurrence_id)
        .await
        .map_err(storage_error)?;
    state
        .service
        .grant_actor_admission(
            &workspace,
            &resolved.occurrence_id,
            &session.auth.claims.sub,
        )
        .await
        .map_err(storage_error)?;
    Ok(Json(resolved))
}

async fn manifest(
    State(state): State<RecordingAppState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<OccurrenceManifest>, MemberError> {
    state.members.authorize(&headers, &workspace).await?;
    Ok(Json(
        state
            .service
            .manifest(&workspace, &id)
            .await
            .map_err(storage_error)?,
    ))
}
async fn transcript(
    State(state): State<RecordingAppState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Revision>, MemberError> {
    state.members.authorize(&headers, &workspace).await?;
    Ok(Json(
        state
            .service
            .manifest(&workspace, &id)
            .await
            .map_err(storage_error)?
            .current
            .ok_or(MemberError::NotFound)?,
    ))
}
async fn audio(
    State(state): State<RecordingAppState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, MemberError> {
    state.members.authorize(&headers, &workspace).await?;
    let (revision, wav) = state
        .service
        .canonical_audio(&workspace, &id)
        .await
        .map_err(storage_error)?;
    Ok((
        [
            (header::CONTENT_TYPE, "audio/wav".to_owned()),
            (header::ETAG, format!("\"{}\"", revision.audio_sha256)),
        ],
        wav,
    )
        .into_response())
}

async fn upload(
    State(state): State<RecordingAppState>,
    Path((workspace, id, source)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(mut request): Json<CaptureUpload>,
) -> Result<Json<OccurrenceManifest>, MemberError> {
    let session = state.members.authorize(&headers, &workspace).await?;
    if !state
        .service
        .actor_admitted(&workspace, &id, &session.auth.claims.sub)
        .await
        .map_err(storage_error)?
    {
        return Err(MemberError::Forbidden);
    }
    request.metadata.uploader_id = session.auth.claims.sub;
    request.metadata.timing_basis = TimingBasis::DeviceClockHint;
    request.metadata.rank = 0;
    let wav = STANDARD
        .decode(&request.wav_base64)
        .map_err(|_| MemberError::Invalid("invalid base64 WAV".into()))?;
    Ok(Json(
        state
            .service
            .upload(&workspace, &id, &source, request.metadata, &wav)
            .await
            .map_err(storage_error)?,
    ))
}

async fn process(
    State(state): State<RecordingAppState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Revision>, MemberError> {
    state.members.authorize(&headers, &workspace).await?;
    let processor = state.processor.as_ref().ok_or(MemberError::Upstream)?;
    match processor.process(&workspace, &id).await {
        Ok(revision) => Ok(Json(revision)),
        Err(error) => {
            if let Some(error) = error.downcast_ref::<RecordingError>() {
                return Err(match error {
                    RecordingError::Conflict => MemberError::Conflict,
                    RecordingError::NotFound => MemberError::NotFound,
                    _ => MemberError::Upstream,
                });
            }
            tracing::warn!("organization transcription processing failed");
            Err(MemberError::Upstream)
        }
    }
}

async fn seal(
    State(state): State<RecordingAppState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Revision>, MemberError> {
    let session = state.members.authorize(&headers, &workspace).await?;
    require_manager(&session.membership)?;
    Ok(Json(
        state
            .service
            .seal(&workspace, &id)
            .await
            .map_err(storage_error)?,
    ))
}
async fn delete_occurrence(
    State(state): State<RecordingAppState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, MemberError> {
    let session = state.members.authorize(&headers, &workspace).await?;
    require_manager(&session.membership)?;
    state
        .service
        .delete_occurrence(&workspace, &id)
        .await
        .map_err(storage_error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn delete_capture(
    State(state): State<RecordingAppState>,
    Path((workspace, id, source)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, MemberError> {
    let session = state.members.authorize(&headers, &workspace).await?;
    let occurrence = state
        .service
        .manifest(&workspace, &id)
        .await
        .map_err(storage_error)?;
    let capture = occurrence
        .captures
        .iter()
        .find(|capture| capture.capture_id == source)
        .ok_or(MemberError::NotFound)?;
    if capture.metadata.uploader_id != session.auth.claims.sub {
        require_manager(&session.membership)?;
    }
    state
        .service
        .delete_capture(&workspace, &id, &source)
        .await
        .map_err(storage_error)?;
    Ok(StatusCode::NO_CONTENT)
}
fn require_manager(membership: &OrganizationMembership) -> Result<(), MemberError> {
    if !matches!(membership.role.as_str(), "owner" | "admin") {
        return Err(MemberError::Forbidden);
    }
    Ok(())
}
fn storage_error(error: RecordingError) -> MemberError {
    match error {
        RecordingError::Invalid(message) => MemberError::Invalid(message),
        RecordingError::Conflict => MemberError::Conflict,
        RecordingError::NotFound => MemberError::NotFound,
        _ => {
            tracing::warn!("organization recording storage failed");
            MemberError::Upstream
        }
    }
}

async fn revision_audio(
    State(state): State<RecordingAppState>,
    Path((workspace, id, revision)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, MemberError> {
    state.members.authorize(&headers, &workspace).await?;
    let (revision, wav) = state
        .service
        .revision_audio(&workspace, &id, &revision)
        .await
        .map_err(storage_error)?;
    Ok((
        [
            (header::CONTENT_TYPE, "audio/wav".to_owned()),
            (header::ETAG, format!("\"{}\"", revision.audio_sha256)),
        ],
        wav,
    )
        .into_response())
}
