use utoipa::OpenApi;

use crate::routes::{
    ConnectionItem, CreateSessionRequest, DeleteConnectionRequest, DeleteConnectionResponse,
    ListConnectionsResponse, SessionMode, SessionResponse, WebhookResponse, WhoAmIItem,
    WhoAmIResponse,
};

#[derive(OpenApi)]
#[openapi(
    paths(
        crate::routes::connect::create_session,
        crate::routes::google_signin::connect,
        crate::routes::google_drive_oauth::start,
        crate::routes::google_drive_oauth::complete,
        crate::routes::google_drive::validate_folder,
        crate::routes::google_drive::prepare_export,
        crate::routes::google_drive::export_markdown,
        crate::routes::google_meet_presence::presence,
        crate::routes::disconnect::delete_connection,
        crate::routes::status::list_connections,
        crate::routes::webhook::nango_webhook,
        crate::routes::whoami::whoami,
    ),
    components(
        schemas(
            CreateSessionRequest,
            crate::routes::google_signin::GoogleSigninRequest,
            crate::routes::google_signin::GoogleSigninResponse,
            SessionMode,
            DeleteConnectionRequest,
            DeleteConnectionResponse,
            SessionResponse,
            ConnectionItem,
            ListConnectionsResponse,
            WebhookResponse,
            WhoAmIItem,
            WhoAmIResponse,
            crate::routes::google_meet_presence::MeetPresenceRequest,
            crate::routes::google_meet_presence::MeetPresenceResponse,
        )
    ),
    tags(
        (name = "nango", description = "Integration management via Nango")
    )
)]
struct ApiDoc;

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
