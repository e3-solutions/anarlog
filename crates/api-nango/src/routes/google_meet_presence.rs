use std::{collections::HashSet, time::Duration};

use anlg_api_auth::AuthContext;
use anlg_nango::OwnedNangoProxy;
use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    GoogleMeet, NangoConnectionError, NangoConnectionState, NangoIntegrationId, state::AppState,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const PRESENCE_DEADLINE: Duration = Duration::from_secs(10);
const ATTENDANCE_DEADLINE: Duration = Duration::from_secs(20);
const MAX_PARTICIPANT_PAGES: usize = 20;

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MeetPresenceRequest {
    pub connection_id: String,
    pub meeting_code: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct MeetPresenceResponse {
    pub present: bool,
    pub account_email: String,
    pub conference_start_time: Option<String>,
    pub meeting_uri: String,
    pub conference_record: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MeetAttendanceRequest {
    pub connection_id: String,
    pub conference_record: String,
}

#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct MeetParticipantSession {
    #[serde(alias = "startTime")]
    pub start_time: String,
    #[serde(alias = "endTime")]
    pub end_time: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct MeetAttendanceResponse {
    pub account_email: String,
    pub conference_start_time: String,
    pub conference_end_time: Option<String>,
    /// None means the connected user's sessions could not be verified.
    pub sessions: Option<Vec<MeetParticipantSession>>,
}

pub enum PresenceError {
    Connection(NangoConnectionError),
    Request(StatusCode, &'static str),
}

impl From<NangoConnectionError> for PresenceError {
    fn from(error: NangoConnectionError) -> Self {
        Self::Connection(error)
    }
}

impl IntoResponse for PresenceError {
    fn into_response(self) -> Response {
        match self {
            Self::Connection(error) => error.into_response(),
            Self::Request(status, message) => {
                anlg_api_error::error_response(status, "google_meet_presence_error", message)
            }
        }
    }
}

fn invalid(message: &'static str) -> PresenceError {
    PresenceError::Request(StatusCode::BAD_REQUEST, message)
}

fn provider_error() -> PresenceError {
    PresenceError::Request(
        StatusCode::BAD_GATEWAY,
        "Could not verify Google Meet presence.",
    )
}

fn validate_connection_id(id: &str) -> Result<(), PresenceError> {
    if id.is_empty()
        || id.len() > 256
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(invalid("Invalid Google Meet connection ID."));
    }
    Ok(())
}

fn validate_meeting_code(code: &str) -> Result<(), PresenceError> {
    if code.len() > 128
        || code.split('-').count() != 3
        || code
            .split('-')
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_lowercase()))
    {
        return Err(invalid("Invalid Google Meet meeting code."));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Person {
    metadata: Option<PersonMetadata>,
}

#[derive(Deserialize)]
struct PersonMetadata {
    #[serde(default)]
    sources: Vec<PersonSource>,
}

#[derive(Deserialize)]
struct PersonSource {
    #[serde(rename = "type")]
    source_type: String,
    id: String,
}

impl Person {
    fn account_id(self) -> Result<String, PresenceError> {
        // A Person resource name can change as profiles and contacts are linked.
        // The ACCOUNT source ID is the Google Account ID accepted by People API.
        let account_ids: HashSet<_> = self
            .metadata
            .ok_or_else(provider_error)?
            .sources
            .into_iter()
            .filter(|source| source.source_type == "ACCOUNT" && !source.id.is_empty())
            .map(|source| source.id)
            .collect();
        let id = if account_ids.len() == 1 {
            account_ids.into_iter().next().unwrap()
        } else {
            return Err(provider_error());
        };
        if id.contains('/') {
            return Err(provider_error());
        }
        Ok(id)
    }
}

#[derive(Deserialize)]
struct GoogleUserInfo {
    email: Option<String>,
    verified_email: Option<bool>,
}

impl GoogleUserInfo {
    fn account_email(self) -> Result<String, PresenceError> {
        if self.verified_email != Some(true) {
            return Err(provider_error());
        }
        let email = self
            .email
            .ok_or_else(provider_error)?
            .trim()
            .to_ascii_lowercase();
        if email.is_empty() || !email.contains('@') || email.contains(char::is_whitespace) {
            return Err(provider_error());
        }
        Ok(email)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Space {
    name: String,
    meeting_uri: String,
    meeting_code: String,
    active_conference: Option<ActiveConference>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveConference {
    conference_record: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConferenceRecord {
    name: String,
    space: String,
    start_time: String,
    end_time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParticipantsPage {
    #[serde(default)]
    participants: Vec<Participant>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Participant {
    name: Option<String>,
    signedin_user: Option<SignedinUser>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParticipantSessionsPage {
    #[serde(default)]
    participant_sessions: Vec<MeetParticipantSession>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct SignedinUser {
    user: String,
}

async fn get<T: DeserializeOwned>(proxy: &OwnedNangoProxy, path: &str) -> Result<T, PresenceError> {
    let response = proxy
        .get(path)
        .map_err(|_| provider_error())?
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|_| provider_error())?;
    if !response.status().is_success() {
        let status = match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => StatusCode::FAILED_DEPENDENCY,
            StatusCode::TOO_MANY_REQUESTS => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::BAD_GATEWAY,
        };
        return Err(PresenceError::Request(
            status,
            "Google Meet presence is unavailable.",
        ));
    }
    response.json().await.map_err(|_| provider_error())
}

async fn connected_identity(proxy: &OwnedNangoProxy) -> Result<(String, String), PresenceError> {
    let person: Person = get(
        &proxy
            .clone()
            .base_url_override("https://people.googleapis.com"),
        "/v1/people/me?personFields=metadata",
    )
    .await?;
    let self_id = person.account_id()?;
    let userinfo: GoogleUserInfo = get(
        &proxy
            .clone()
            .base_url_override("https://www.googleapis.com"),
        "/oauth2/v1/userinfo?alt=json",
    )
    .await?;
    let account_email = userinfo.account_email()?;
    Ok((self_id, account_email))
}

async fn check_presence(
    proxy: OwnedNangoProxy,
    meeting_code: &str,
) -> Result<MeetPresenceResponse, PresenceError> {
    let (self_id, account_email) = connected_identity(&proxy).await?;

    let meet = proxy.base_url_override("https://meet.googleapis.com");
    let space: Space = get(&meet, &format!("/v2/spaces/{meeting_code}")).await?;
    if space.meeting_code != meeting_code
        || !space.name.starts_with("spaces/")
        || space.name.len() <= "spaces/".len()
        || space.meeting_uri != format!("https://meet.google.com/{meeting_code}")
    {
        return Err(provider_error());
    }
    let mut result = MeetPresenceResponse {
        present: false,
        account_email,
        conference_start_time: None,
        meeting_uri: space.meeting_uri,
        conference_record: None,
    };
    let Some(active) = space.active_conference else {
        return Ok(result);
    };
    if !valid_resource(&active.conference_record, "conferenceRecords/") {
        return Err(provider_error());
    }
    let record: ConferenceRecord = get(&meet, &format!("/v2/{}", active.conference_record)).await?;
    if record.name != active.conference_record
        || record.space != space.name
        || record.start_time.is_empty()
        || record.end_time.is_some()
    {
        return Err(provider_error());
    }
    result.conference_start_time = Some(record.start_time);

    let self_name = format!("users/{self_id}");
    let mut seen_tokens = HashSet::new();
    let mut page_token: Option<String> = None;
    for _ in 0..MAX_PARTICIPANT_PAGES {
        let mut path = format!(
            "/v2/{}/participants?pageSize=250&filter={}",
            active.conference_record,
            urlencoding::encode("latest_end_time IS NULL")
        );
        if let Some(token) = &page_token {
            path.push_str("&pageToken=");
            path.push_str(&urlencoding::encode(token));
        }
        let page: ParticipantsPage = get(&meet, &path).await?;
        if page.participants.iter().any(|p| {
            p.signedin_user
                .as_ref()
                .is_some_and(|user| user.user == self_name)
        }) {
            result.present = true;
            result.conference_record = Some(active.conference_record);
            return Ok(result);
        }
        match page.next_page_token {
            Some(token) if !token.is_empty() => {
                if !seen_tokens.insert(token.clone()) {
                    return Err(provider_error());
                }
                page_token = Some(token);
            }
            _ => return Ok(result),
        }
    }
    Err(provider_error())
}

fn valid_resource(value: &str, prefix: &str) -> bool {
    value.len() <= 512
        && value.strip_prefix(prefix).is_some_and(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

async fn read_attendance(
    proxy: OwnedNangoProxy,
    conference_record: &str,
) -> Result<MeetAttendanceResponse, PresenceError> {
    let (self_id, account_email) = connected_identity(&proxy).await?;
    let meet = proxy.base_url_override("https://meet.googleapis.com");
    let record: ConferenceRecord = get(&meet, &format!("/v2/{conference_record}")).await?;
    if record.name != conference_record || record.start_time.is_empty() {
        return Err(provider_error());
    }
    let self_name = format!("users/{self_id}");
    let mut page_token: Option<String> = None;
    let mut seen_tokens = HashSet::new();
    let mut participant_name = None;
    let mut participants_complete = false;
    for _ in 0..MAX_PARTICIPANT_PAGES {
        let mut path = format!("/v2/{conference_record}/participants?pageSize=250");
        if let Some(token) = &page_token {
            path.push_str("&pageToken=");
            path.push_str(&urlencoding::encode(token));
        }
        let page: ParticipantsPage = get(&meet, &path).await?;
        for participant in page.participants {
            if participant
                .signedin_user
                .as_ref()
                .is_some_and(|user| user.user == self_name)
            {
                let name = participant.name.ok_or_else(provider_error)?;
                if !valid_resource(&name, &format!("{conference_record}/participants/")) {
                    return Err(provider_error());
                }
                participant_name = Some(name);
                break;
            }
        }
        if participant_name.is_some() {
            break;
        }
        match page.next_page_token {
            Some(token) if !token.is_empty() => {
                if !seen_tokens.insert(token.clone()) {
                    return Err(provider_error());
                }
                page_token = Some(token);
            }
            _ => {
                participants_complete = true;
                break;
            }
        }
    }
    let Some(participant_name) = participant_name else {
        if !participants_complete {
            return Err(provider_error());
        }
        return Ok(MeetAttendanceResponse {
            account_email,
            conference_start_time: record.start_time,
            conference_end_time: record.end_time,
            sessions: None,
        });
    };
    let mut sessions = Vec::new();
    let mut page_token: Option<String> = None;
    let mut seen_tokens = HashSet::new();
    for _ in 0..MAX_PARTICIPANT_PAGES {
        let mut path = format!("/v2/{participant_name}/participantSessions?pageSize=250");
        if let Some(token) = &page_token {
            path.push_str("&pageToken=");
            path.push_str(&urlencoding::encode(token));
        }
        let page: ParticipantSessionsPage = get(&meet, &path).await?;
        for session in page.participant_sessions {
            let start = chrono::DateTime::parse_from_rfc3339(&session.start_time)
                .map_err(|_| provider_error())?;
            if let Some(end_time) = &session.end_time {
                let end =
                    chrono::DateTime::parse_from_rfc3339(end_time).map_err(|_| provider_error())?;
                if end < start {
                    return Err(provider_error());
                }
            }
            sessions.push(session);
        }
        match page.next_page_token {
            Some(token) if !token.is_empty() => {
                if !seen_tokens.insert(token.clone()) {
                    return Err(provider_error());
                }
                page_token = Some(token);
            }
            _ => {
                return Ok(MeetAttendanceResponse {
                    account_email,
                    conference_start_time: record.start_time,
                    conference_end_time: record.end_time,
                    sessions: (!sessions.is_empty()).then_some(sessions),
                });
            }
        }
    }
    Err(provider_error())
}

#[utoipa::path(post, path = "/google-meet/attendance", operation_id = "google_meet_attendance", request_body = MeetAttendanceRequest, responses((status = 200, body = MeetAttendanceResponse)), tag = "nango")]
pub async fn attendance(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<MeetAttendanceRequest>,
) -> Result<Json<MeetAttendanceResponse>, PresenceError> {
    validate_connection_id(&request.connection_id)?;
    if !valid_resource(&request.conference_record, "conferenceRecords/") {
        return Err(invalid("Invalid Google Meet conference record."));
    }
    let proxy = NangoConnectionState::from_config(&state.config)
        .build_http_client(
            &auth.token,
            &auth.claims.sub,
            GoogleMeet::ID,
            &request.connection_id,
        )
        .await?
        .into_proxy()
        .retries(0);
    Ok(Json(
        tokio::time::timeout(
            ATTENDANCE_DEADLINE,
            read_attendance(proxy, &request.conference_record),
        )
        .await
        .map_err(|_| provider_error())??,
    ))
}

#[utoipa::path(post, path = "/google-meet/presence", operation_id = "google_meet_presence", request_body = MeetPresenceRequest, responses((status = 200, body = MeetPresenceResponse)), tag = "nango")]
pub async fn presence(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<MeetPresenceRequest>,
) -> Result<Json<MeetPresenceResponse>, PresenceError> {
    validate_connection_id(&request.connection_id)?;
    validate_meeting_code(&request.meeting_code)?;
    let proxy = NangoConnectionState::from_config(&state.config)
        .build_http_client(
            &auth.token,
            &auth.claims.sub,
            GoogleMeet::ID,
            &request.connection_id,
        )
        .await?
        .into_proxy()
        .retries(0);
    Ok(Json(
        tokio::time::timeout(
            PRESENCE_DEADLINE,
            check_presence(proxy, &request.meeting_code),
        )
        .await
        .map_err(|_| provider_error())??,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request};
    use serde_json::{Value, json};
    use tower::ServiceExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path, query_param},
    };

    async fn setup(owned: bool) -> (MockServer, Router) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/v1/nango_connections"))
            .and(query_param("user_id", "eq.user-1"))
            .and(query_param("connection_id", "eq.connection"))
            .and(query_param("integration_id", "eq.google-meet"))
            .respond_with(ResponseTemplate::new(200).set_body_json(if owned {
                json!([{"status":"connected"}])
            } else {
                json!([])
            }))
            .mount(&server)
            .await;
        let config = crate::NangoConfig::for_test(&server.uri(), &server.uri());
        let auth = AuthContext {
            token: "user-token".into(),
            claims: serde_json::from_value(json!({"sub":"user-1"})).unwrap(),
        };
        (server, crate::session_router(config).layer(Extension(auth)))
    }

    async fn post(app: Router, code: &str) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::post("/google-meet/presence")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"connection_id":"connection","meeting_code":code}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn post_attendance(app: Router, record: &str) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::post("/google-meet/attendance")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"connection_id":"connection","conference_record":record})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn mock_identity(server: &MockServer) {
        mock_person(server, person()).await;
        mock_userinfo(
            server,
            json!({"email":"E3@example.com","verified_email":true}),
        )
        .await;
        mock_record(server).await;
    }

    async fn mock_person(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path("/proxy/v1/people/me"))
            .and(query_param("personFields", "metadata"))
            .and(header("Connection-Id", "connection"))
            .and(header("Provider-Config-Key", "google-meet"))
            .and(header("Base-Url-Override", "https://people.googleapis.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    fn person() -> Value {
        json!({
            "resourceName":"people/different-aggregate-id",
            "metadata":{"sources":[{"type":"PROFILE","id":"profile-id"},{"type":"ACCOUNT","id":"12345"}]},
            "emailAddresses":[{"value":"e3@example.com","metadata":{"primary":true,"source":{"type":"DOMAIN_PROFILE","id":"workspace-profile"}}}]
        })
    }

    async fn mock_userinfo(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path("/proxy/oauth2/v1/userinfo"))
            .and(query_param("alt", "json"))
            .and(header("Connection-Id", "connection"))
            .and(header("Base-Url-Override", "https://www.googleapis.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mock_space(server: &MockServer, active: bool) {
        Mock::given(method("GET"))
            .and(path("/proxy/v2/spaces/abc-defg-hij"))
            .and(header("Base-Url-Override", "https://meet.googleapis.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name":"spaces/space1", "meetingUri":"https://meet.google.com/abc-defg-hij",
                "meetingCode":"abc-defg-hij",
                "activeConference": if active { json!({"conferenceRecord":"conferenceRecords/record1"}) } else { Value::Null }
            })))
            .mount(server).await;
    }

    async fn mock_record(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name":"conferenceRecords/record1", "space":"spaces/space1",
                "startTime":"2026-10-15T10:00:00Z"
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn rejects_unowned_connection_before_google_requests() {
        let (server, app) = setup(false).await;
        let (status, _) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejects_invalid_code_before_connection_lookup() {
        let (server, app) = setup(true).await;
        let (status, _) = post(app, "abc/../../other").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn matches_account_source_id_not_person_resource_name() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(
            &server,
            json!({"email":"E3@example.com","verified_email":true}),
        )
        .await;
        mock_space(&server, true).await;
        mock_record(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .and(query_param("filter", "latest_end_time IS NULL"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"signedinUser":{"user":"users/other"}},{"signedinUser":{"user":"users/12345"}}]
            })))
            .mount(&server).await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["present"], true);
        assert_eq!(value["account_email"], "e3@example.com");
        assert_eq!(value["conference_start_time"], "2026-10-15T10:00:00Z");
    }

    #[tokio::test]
    async fn other_active_participant_does_not_count_as_self() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(
            &server,
            json!({"email":"E3@example.com","verified_email":true}),
        )
        .await;
        mock_space(&server, true).await;
        mock_record(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"signedinUser":{"user":"users/other"}}]
            })))
            .mount(&server)
            .await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["present"], false);
        assert_eq!(value["conference_start_time"], "2026-10-15T10:00:00Z");
    }

    #[tokio::test]
    async fn checks_next_page_before_reporting_absent() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(
            &server,
            json!({"email":"E3@example.com","verified_email":true}),
        )
        .await;
        mock_space(&server, true).await;
        mock_record(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .and(query_param("filter", "latest_end_time IS NULL"))
            .and(query_param("pageToken", "next"))
            .with_priority(1)
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"signedinUser":{"user":"users/12345"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .and(query_param("filter", "latest_end_time IS NULL"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"signedinUser":{"user":"users/other"}}],
                "nextPageToken":"next"
            })))
            .mount(&server)
            .await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["present"], true);
    }

    #[tokio::test]
    async fn no_active_conference_is_absent() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(
            &server,
            json!({"email":"E3@example.com","verified_email":true}),
        )
        .await;
        mock_space(&server, false).await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["present"], false);
        assert!(value["conference_start_time"].is_null());
    }

    #[tokio::test]
    async fn google_denial_is_error_not_absence() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(
            &server,
            json!({"email":"E3@example.com","verified_email":true}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/spaces/abc-defg-hij"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::FAILED_DEPENDENCY);
        assert!(value.get("present").is_none());
    }

    #[tokio::test]
    async fn missing_account_source_fails_closed() {
        let (server, app) = setup(true).await;
        mock_person(&server, json!({
            "resourceName":"people/12345", "metadata":{"sources":[{"type":"PROFILE","id":"12345"}]},
            "emailAddresses":[{"value":"e3@example.com","metadata":{"source":{"type":"PROFILE","id":"12345"}}}]
        })).await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(value.get("present").is_none());
    }

    #[tokio::test]
    async fn missing_userinfo_email_fails_closed() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(&server, json!({"name":"E3 employee"})).await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(value.get("present").is_none());
    }

    #[tokio::test]
    async fn unverified_userinfo_email_fails_closed() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(
            &server,
            json!({"email":"e3@example.com","verified_email":false}),
        )
        .await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(value.get("present").is_none());
    }

    #[tokio::test]
    async fn malformed_google_response_is_error_not_absence() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(
            &server,
            json!({"email":"E3@example.com","verified_email":true}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/spaces/abc-defg-hij"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"meetingCode":"abc-defg-hij"})),
            )
            .mount(&server)
            .await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(value.get("present").is_none());
    }

    #[tokio::test]
    async fn later_page_error_is_not_reported_as_absence() {
        let (server, app) = setup(true).await;
        mock_person(&server, person()).await;
        mock_userinfo(
            &server,
            json!({"email":"E3@example.com","verified_email":true}),
        )
        .await;
        mock_space(&server, true).await;
        mock_record(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .and(query_param("pageToken", "next"))
            .with_priority(1)
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"signedinUser":{"user":"users/other"}}],
                "nextPageToken":"next"
            })))
            .mount(&server)
            .await;
        let (status, value) = post(app, "abc-defg-hij").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(value.get("present").is_none());
    }

    #[tokio::test]
    async fn attendance_uses_only_connected_users_participant_and_all_session_pages() {
        let (server, app) = setup(true).await;
        mock_identity(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .and(query_param("pageToken", "next"))
            .with_priority(1)
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"name":"conferenceRecords/record1/participants/self",
                    "signedinUser":{"user":"users/12345"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"name":"conferenceRecords/record1/participants/other",
                    "signedinUser":{"user":"users/other"}}],
                "nextPageToken":"next"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/proxy/v2/conferenceRecords/record1/participants/self/participantSessions",
            ))
            .and(query_param("pageToken", "more"))
            .with_priority(1)
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participantSessions":[{"startTime":"2026-10-15T10:20:00Z",
                    "endTime":"2026-10-15T10:50:00Z"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/proxy/v2/conferenceRecords/record1/participants/self/participantSessions",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participantSessions":[{"startTime":"2026-10-15T10:00:00Z",
                    "endTime":"2026-10-15T10:10:00Z"}],
                "nextPageToken":"more"
            })))
            .mount(&server)
            .await;
        let (status, value) = post_attendance(app, "conferenceRecords/record1").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["account_email"], "e3@example.com");
        assert_eq!(value["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(value["sessions"][0]["start_time"], "2026-10-15T10:00:00Z");
        assert_eq!(value["sessions"][1]["end_time"], "2026-10-15T10:50:00Z");
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .all(|request| !request.url.path().contains("/other/participantSessions"))
        );
    }

    #[tokio::test]
    async fn attendance_returns_unknown_when_connected_user_is_absent() {
        let (server, app) = setup(true).await;
        mock_identity(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"name":"conferenceRecords/record1/participants/other",
                    "signedinUser":{"user":"users/other"}}]
            })))
            .mount(&server)
            .await;
        let (status, value) = post_attendance(app, "conferenceRecords/record1").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert!(value["sessions"].is_null());
    }

    #[tokio::test]
    async fn attendance_does_not_return_partial_sessions_after_page_error() {
        let (server, app) = setup(true).await;
        mock_identity(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"name":"conferenceRecords/record1/participants/self",
                    "signedinUser":{"user":"users/12345"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/proxy/v2/conferenceRecords/record1/participants/self/participantSessions",
            ))
            .and(query_param("pageToken", "more"))
            .with_priority(1)
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/proxy/v2/conferenceRecords/record1/participants/self/participantSessions",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participantSessions":[{"startTime":"2026-10-15T10:00:00Z"}],
                "nextPageToken":"more"
            })))
            .mount(&server)
            .await;
        let (status, value) = post_attendance(app, "conferenceRecords/record1").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
        assert!(value.get("sessions").is_none());
    }

    #[tokio::test]
    async fn attendance_rejects_invalid_session_time() {
        let (server, app) = setup(true).await;
        mock_identity(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"name":"conferenceRecords/record1/participants/self",
                    "signedinUser":{"user":"users/12345"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/proxy/v2/conferenceRecords/record1/participants/self/participantSessions",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participantSessions":[{"startTime":"2026-10-15T10:20:00Z",
                    "endTime":"2026-10-15T10:10:00Z"}]
            })))
            .mount(&server)
            .await;
        let (status, value) = post_attendance(app, "conferenceRecords/record1").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
        assert!(value.get("sessions").is_none());
    }

    #[tokio::test]
    async fn attendance_preserves_open_session_without_inventing_leave_time() {
        let (server, app) = setup(true).await;
        mock_identity(&server).await;
        Mock::given(method("GET"))
            .and(path("/proxy/v2/conferenceRecords/record1/participants"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"name":"conferenceRecords/record1/participants/self",
                    "signedinUser":{"user":"users/12345"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/proxy/v2/conferenceRecords/record1/participants/self/participantSessions",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participantSessions":[{"startTime":"2026-10-15T10:20:00Z"}]
            })))
            .mount(&server)
            .await;
        let (status, value) = post_attendance(app, "conferenceRecords/record1").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert!(value["sessions"][0]["end_time"].is_null());
    }

    #[tokio::test]
    async fn attendance_rejects_unowned_connection_and_bad_record() {
        let (server, app) = setup(false).await;
        let (status, _) = post_attendance(app, "conferenceRecords/record1").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);

        let (server, app) = setup(true).await;
        let (status, _) = post_attendance(app, "conferenceRecords/../other").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
