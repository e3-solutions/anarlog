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
    signedin_user: Option<SignedinUser>,
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

pub struct VerifiedMeetOccurrence {
    pub conference_record: String,
    pub start_time: String,
}

pub async fn resolve_verified_active_conference(
    proxy: OwnedNangoProxy,
    meeting_code: &str,
) -> Result<VerifiedMeetOccurrence, PresenceError> {
    validate_meeting_code(meeting_code)?;
    let (presence, occurrence) = check_presence_details(proxy, meeting_code).await?;
    if !presence.present {
        return Err(provider_error());
    }
    occurrence.ok_or_else(provider_error)
}

async fn check_presence(
    proxy: OwnedNangoProxy,
    meeting_code: &str,
) -> Result<MeetPresenceResponse, PresenceError> {
    Ok(check_presence_details(proxy, meeting_code).await?.0)
}

async fn check_presence_details(
    proxy: OwnedNangoProxy,
    meeting_code: &str,
) -> Result<(MeetPresenceResponse, Option<VerifiedMeetOccurrence>), PresenceError> {
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
    };
    let Some(active) = space.active_conference else {
        return Ok((result, None));
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
    result.conference_start_time = Some(record.start_time.clone());

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
            return Ok((
                result,
                Some(VerifiedMeetOccurrence {
                    conference_record: record.name,
                    start_time: record.start_time,
                }),
            ));
        }
        match page.next_page_token {
            Some(token) if !token.is_empty() => {
                if !seen_tokens.insert(token.clone()) {
                    return Err(provider_error());
                }
                page_token = Some(token);
            }
            _ => return Ok((result, None)),
        }
    }
    Err(provider_error())
}

fn valid_resource(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
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
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "participants":[{"signedinUser":{"user":"users/12345"}}]
            })))
            .with_priority(1)
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
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
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
}
