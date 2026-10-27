use std::sync::Arc;

use anlg_api_auth::{AuthContext, AuthState};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use tower_http::limit::RequestBodyLimitLayer;

use crate::{
    recording_identity::{
        AdmissionRequest, RecordingIdentityResolver, ResolvedOccurrence, resolve_local_teams,
    },
    recording_members::{
        MemberError, OrganizationMembership, ProviderConnection, RecordingMemberships,
        RecordingPolicy,
    },
    recording_transcription::RecordingProcessor,
    shared_recordings::{
        CaptureUpload, OccurrenceManifest, Provider, RecordingError, SharedRecordingService,
        TimingBasis,
    },
};

#[derive(Clone)]
pub struct RecordingAppState {
    pub service: Arc<SharedRecordingService>,
    pub members: RecordingMemberships,
    pub identities: Option<RecordingIdentityResolver>,
    pub processor: Option<Arc<RecordingProcessor>>,
}

const PATHS: [&str; 9] = [
    "/v1/shared-recordings/me",
    "/v1/shared-recordings/enroll",
    "/v1/orgs/{workspace}/shared-recordings",
    "/v1/orgs/{workspace}/shared-recordings/admit",
    "/v1/orgs/{workspace}/shared-recordings/{id}",
    "/v1/orgs/{workspace}/shared-recordings/{id}/captures/{source}/audio",
    "/v1/orgs/{workspace}/shared-recordings/{id}/captures/{source}",
    "/v1/orgs/{workspace}/shared-recordings/{id}/process",
    "/v1/orgs/{workspace}/shared-recordings/{id}/seal",
];

pub fn forwarding_router() -> Router {
    PATHS.into_iter().fold(Router::new(), |router, path| {
        router.route(path, any(|| async { StatusCode::NOT_FOUND }))
    })
}

pub fn router(state: RecordingAppState, auth: AuthState) -> Router {
    let [
        account,
        enrollment,
        library_path,
        admission,
        occurrence,
        original_audio,
        capture,
        processing,
        sealing,
    ] = PATHS;
    Router::new()
        .route(account, get(me))
        .route(enrollment, post(enroll))
        .route(library_path, get(library))
        .route(admission, post(admit))
        .route(occurrence, get(manifest).delete(delete_occurrence))
        .route(original_audio, get(audio))
        .route(capture, post(upload).delete(delete_capture))
        .route(processing, post(process))
        .route(sealing, post(seal))
        .layer(DefaultBodyLimit::max(140 * 1024 * 1024))
        .with_state(state)
        .route_layer(middleware::from_fn_with_state(
            auth,
            anlg_api_auth::require_auth,
        ))
        .layer(RequestBodyLimitLayer::new(140 * 1024 * 1024))
        .layer(middleware::from_fn(response_headers))
}

fn wav_response(wav: Vec<u8>, audio_sha256: &str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "audio/wav".to_owned()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::ETAG, format!("\"{audio_sha256}\"")),
        ],
        wav,
    )
        .into_response()
}

async fn response_headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().remove(header::SERVER);
    response
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    user_id: String,
    workspaces: Vec<OrganizationMembership>,
    connections: Vec<ProviderConnection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy: Option<RecordingPolicy>,
}
async fn me(
    State(state): State<RecordingAppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Account>, MemberError> {
    let workspaces = state.members.memberships(&auth).await?;
    let mut connections = state.members.connections(&auth).await?;
    if let Some(identities) = &state.identities {
        let mut calendars = Vec::new();
        for connection in &connections {
            if connection.integration_id == "google-meet"
                && connection.status != "reconnect_required"
                && !connections.iter().any(|item| {
                    item.integration_id == "google-calendar"
                        && item.connection_id == connection.connection_id
                })
                && identities
                    .google_calendar_access(&auth, &connection.connection_id)
                    .await
            {
                calendars.push(ProviderConnection {
                    integration_id: "google-calendar".into(),
                    connection_id: connection.connection_id.clone(),
                    status: connection.status.clone(),
                });
            }
        }
        connections.extend(calendars);
    }
    let policy = state.members.policy(&auth, &workspaces).await?;
    Ok(Json(Account {
        user_id: auth.claims.sub,
        workspaces,
        connections,
        policy,
    }))
}

async fn enroll(
    State(state): State<RecordingAppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Account>, MemberError> {
    state.members.enroll(&auth).await?;
    me(State(state), Extension(auth)).await
}

#[derive(Serialize)]
struct Library {
    items: Vec<OccurrenceManifest>,
}
async fn library(
    State(state): State<RecordingAppState>,
    Path(workspace): Path<String>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Library>, MemberError> {
    state.members.authorize(&auth, &workspace).await?;
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
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<AdmissionRequest>,
) -> Result<Json<ResolvedOccurrence>, MemberError> {
    state.members.authorize(&auth, &workspace).await?;
    let mut resolved = if request.provider == Provider::MicrosoftTeams {
        resolve_local_teams(&auth, &request)?
    } else {
        state
            .identities
            .as_ref()
            .ok_or(MemberError::Unresolved)?
            .resolve(&auth, &request)
            .await?
    };
    resolved.provider_started_at_ms = state
        .service
        .admit_actor(
            &workspace,
            &resolved.occurrence_id,
            &auth.claims.sub,
            resolved.provider.clone(),
            &resolved.provider_occurrence_id,
            resolved.provider_started_at_ms,
        )
        .await
        .map_err(storage_error)?;
    Ok(Json(resolved))
}

async fn manifest(
    State(state): State<RecordingAppState>,
    Path((workspace, id)): Path<(String, String)>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<OccurrenceManifest>, MemberError> {
    state.members.authorize(&auth, &workspace).await?;
    Ok(Json(
        state
            .service
            .manifest(&workspace, &id)
            .await
            .map_err(storage_error)?,
    ))
}
async fn audio(
    State(state): State<RecordingAppState>,
    Path((workspace, id, source)): Path<(String, String, String)>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Response, MemberError> {
    state.members.authorize(&auth, &workspace).await?;
    let (capture, wav) = state
        .service
        .source_audio(&workspace, &id, &source)
        .await
        .map_err(storage_error)?;
    Ok(wav_response(wav, &capture.metadata.sha256))
}

async fn upload(
    State(state): State<RecordingAppState>,
    Path((workspace, id, source)): Path<(String, String, String)>,
    Extension(auth): Extension<AuthContext>,
    Json(mut request): Json<CaptureUpload>,
) -> Result<Json<OccurrenceManifest>, MemberError> {
    state.members.authorize(&auth, &workspace).await?;
    if !state
        .service
        .actor_admitted(&workspace, &id, &auth.claims.sub)
        .await
        .map_err(storage_error)?
    {
        return Err(MemberError::Forbidden);
    }
    request.metadata.uploader_id = auth.claims.sub;
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
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<OccurrenceManifest>, MemberError> {
    state.members.authorize(&auth, &workspace).await?;
    let processor = state.processor.as_ref().ok_or(MemberError::Upstream)?;
    match processor.process(&workspace, &id).await {
        Ok(manifest) => Ok(Json(manifest)),
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
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<OccurrenceManifest>, MemberError> {
    let membership = state.members.authorize(&auth, &workspace).await?;
    require_manager(&membership)?;
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
    Extension(auth): Extension<AuthContext>,
) -> Result<StatusCode, MemberError> {
    let membership = state.members.authorize(&auth, &workspace).await?;
    require_manager(&membership)?;
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
    Extension(auth): Extension<AuthContext>,
) -> Result<StatusCode, MemberError> {
    let membership = state.members.authorize(&auth, &workspace).await?;
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
    if capture.metadata.uploader_id != auth.claims.sub {
        require_manager(&membership)?;
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
