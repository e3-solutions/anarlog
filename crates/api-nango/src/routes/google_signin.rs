use anlg_api_auth::AuthContext;
use anlg_nango::{CreateConnectionRequest, EndUser};
use axum::{Extension, Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::{
    error::{NangoError, Result},
    state::AppState,
};

const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/meetings.space.readonly",
    "https://www.googleapis.com/auth/calendar.readonly",
];

#[derive(Deserialize, ToSchema)]
pub struct GoogleSigninRequest {
    pub provider_refresh_token: String,
}

#[derive(Serialize, ToSchema)]
pub struct GoogleSigninResponse {
    pub connected: bool,
    pub connection_id: String,
}

fn invalid() -> NangoError {
    NangoError::BadRequest("Google connection could not be verified. Sign in again and allow Calendar and Meet access.".to_string())
}

fn verified_identity(user: &Value, google: &Value, user_id: &str, domain: &str) -> bool {
    let Some(email) = user.get("email").and_then(Value::as_str) else {
        return false;
    };
    let Some(sub) = google.get("sub").and_then(Value::as_str) else {
        return false;
    };
    user.get("id").and_then(Value::as_str) == Some(user_id)
        && user
            .get("email_confirmed_at")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        && user.get("is_anonymous").and_then(Value::as_bool) == Some(false)
        && email
            .rsplit_once('@')
            .is_some_and(|(_, value)| value.eq_ignore_ascii_case(domain))
        && google.get("email_verified").and_then(Value::as_bool) == Some(true)
        && google
            .get("email")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case(email))
        && user
            .get("identities")
            .and_then(Value::as_array)
            .is_some_and(|identities| {
                identities.iter().any(|identity| {
                    identity.get("provider").and_then(Value::as_str) == Some("google")
                        && identity
                            .pointer("/identity_data/sub")
                            .and_then(Value::as_str)
                            == Some(sub)
                })
            })
}

#[utoipa::path(
    post, path = "/google-signin", request_body = GoogleSigninRequest,
    responses((status = 200, body = GoogleSigninResponse), (status = 400, description = "Google permission or identity verification failed"), (status = 401, description = "Unauthorized")),
    tag = "nango"
)]
pub async fn connect(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<GoogleSigninRequest>,
) -> Result<Json<GoogleSigninResponse>> {
    let setting = |key| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                NangoError::Internal("Google sign-in connection is not configured".to_string())
            })
    };
    import_connection(
        state,
        auth,
        body,
        GoogleSigninSettings {
            client_id: setting("E3_GOOGLE_SIGNIN_CLIENT_ID")?,
            client_secret: setting("E3_GOOGLE_SIGNIN_CLIENT_SECRET")?,
            domain: setting("E3_RECORDING_ENROLLMENT_DOMAIN")?,
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            userinfo_url: "https://openidconnect.googleapis.com/v1/userinfo".to_string(),
        },
    )
    .await
}

#[derive(Clone)]
struct GoogleSigninSettings {
    client_id: String,
    client_secret: String,
    domain: String,
    token_url: String,
    userinfo_url: String,
}

async fn import_connection(
    state: AppState,
    auth: AuthContext,
    body: GoogleSigninRequest,
    settings: GoogleSigninSettings,
) -> Result<Json<GoogleSigninResponse>> {
    if body.provider_refresh_token.is_empty() || body.provider_refresh_token.len() > 8192 {
        return Err(invalid());
    }
    let GoogleSigninSettings {
        client_id,
        client_secret,
        domain,
        token_url,
        userinfo_url,
    } = settings;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| invalid())?;
    let user_response = http
        .get(format!(
            "{}/auth/v1/user",
            state.config.supabase_url.trim_end_matches('/')
        ))
        .bearer_auth(&auth.token)
        .header("apikey", &state.config.supabase_anon_key)
        .send()
        .await
        .map_err(|_| invalid())?;
    if !user_response.status().is_success() {
        return Err(NangoError::Auth("Session expired".to_string()));
    }
    let user: Value = user_response.json().await.map_err(|_| invalid())?;

    // Refresh first so the identity and scopes belong to the credential Nango will retain.
    let refresh = http
        .post(token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", body.provider_refresh_token.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
        ])
        .send()
        .await
        .map_err(|_| invalid())?;
    if !refresh.status().is_success() {
        return Err(invalid());
    }
    let refreshed: Value = refresh.json().await.map_err(|_| invalid())?;
    let access_token = refreshed
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let scope = refreshed
        .get("scope")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if !SCOPES
        .iter()
        .all(|required| scope.split_whitespace().any(|value| value == *required))
    {
        return Err(invalid());
    }
    let google = http
        .get(userinfo_url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| invalid())?;
    if !google.status().is_success() {
        return Err(invalid());
    }
    let google: Value = google.json().await.map_err(|_| invalid())?;
    if !verified_identity(&user, &google, &auth.claims.sub, &domain) {
        return Err(NangoError::Forbidden(
            "Google account must match your confirmed E3 sign-in account".to_string(),
        ));
    }
    let expires = refreshed
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0 && *value <= 86400)
        .ok_or_else(invalid)?;
    let existing = state
        .supabase
        .lookup_connection(&auth.token, &auth.claims.sub, "google-meet")
        .await?;
    let connection_id = existing
        .as_ref()
        .map(|row| row.connection_id.clone())
        .unwrap_or_else(|| format!("e3-google-{}", auth.claims.sub));
    match state
        .nango
        .get_connection(&connection_id, "google-meet")
        .await
    {
        Ok(connection)
            if connection
                .end_user
                .as_ref()
                .is_some_and(|owner| owner.id == auth.claims.sub) => {}
        Err(anlg_nango::Error::Api(404, _)) => {}
        Ok(_) => {
            return Err(NangoError::Forbidden(
                "Google connection belongs to another account".to_string(),
            ));
        }
        Err(error) => return Err(error.into()),
    }
    let refresh_token = refreshed
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or(&body.provider_refresh_token);
    let connection = state.nango.create_connection(CreateConnectionRequest {
        provider_config_key: "google-meet".to_string(), connection_id: Some(connection_id.clone()),
        credentials: json!({"type":"OAUTH2", "access_token":access_token, "refresh_token":refresh_token,
            "expires_at":(chrono::Utc::now() + chrono::Duration::seconds(expires)).to_rfc3339(),
            "config_override":{"client_id":client_id,"client_secret":client_secret}}),
        metadata: None, connection_config: None,
        end_user: Some(EndUser { id: auth.claims.sub.clone(), email: user.get("email").and_then(Value::as_str).map(str::to_string), display_name: None, tags: None }),
    }).await?;
    if connection.connection_id != connection_id
        || connection
            .end_user
            .as_ref()
            .is_none_or(|owner| owner.id != auth.claims.sub)
    {
        return Err(NangoError::Internal(
            "Imported connection ownership mismatch".to_string(),
        ));
    }
    state
        .supabase
        .upsert_connection(
            &auth.claims.sub,
            "google-meet",
            &connection_id,
            &connection.provider,
        )
        .await?;
    Ok(Json(GoogleSigninResponse {
        connected: true,
        connection_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
        response::IntoResponse,
        routing::post,
    };
    use tower::ServiceExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, body_string_contains, header, method, path},
    };

    #[tokio::test]
    async fn signin_imports_only_matching_google_credentials_with_calendar_and_meet_access() {
        for (google_sub, scopes, existing_owner, expected) in [
            ("google-user", SCOPES.join(" "), None, 200),
            ("google-user", SCOPES.join(" "), Some("another-owner"), 403),
            ("another-google-user", SCOPES.join(" "), None, 403),
            ("google-user", SCOPES[0].to_string(), None, 400),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET")).and(path("/auth/v1/user"))
                .and(header("Authorization", "Bearer session-token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id":"user", "email":"person@e3group.ai", "email_confirmed_at":"2026-10-27", "is_anonymous":false,
                    "identities":[{"provider":"google", "identity_data":{"sub":"google-user"}}]
                }))).mount(&server).await;
            Mock::given(method("POST")).and(path("/token"))
                .and(body_string_contains("client_id=signin-client"))
                .and(body_string_contains("client_secret=signin-secret"))
                .and(body_string_contains("refresh_token=initial-refresh"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "access_token":"fresh-access", "refresh_token":"rotated-refresh", "scope":scopes, "expires_in":3600
                }))).mount(&server).await;
            Mock::given(method("GET"))
                .and(path("/userinfo"))
                .and(header("Authorization", "Bearer fresh-access"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "sub":google_sub, "email":"person@e3group.ai", "email_verified":true
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/rest/v1/nango_connections"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
            Mock::given(method("GET")).and(path("/connections/e3-google-user"))
                .respond_with(if let Some(owner) = existing_owner {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "id":1, "connection_id":"e3-google-user", "provider_config_key":"google-meet", "provider":"google-meet",
                        "errors":[], "end_user":{"id":owner}, "metadata":{}, "connection_config":{},
                        "created_at":"now", "updated_at":"now", "last_fetched_at":"now", "credentials":{}
                    }))
                } else { ResponseTemplate::new(404) }).mount(&server).await;
            Mock::given(method("POST")).and(path("/connections"))
                .and(body_partial_json(json!({
                    "provider_config_key":"google-meet", "connection_id":"e3-google-user",
                    "credentials":{"type":"OAUTH2", "access_token":"fresh-access", "refresh_token":"rotated-refresh",
                        "config_override":{"client_id":"signin-client", "client_secret":"signin-secret"}},
                    "end_user":{"id":"user", "email":"person@e3group.ai"}
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id":1, "connection_id":"e3-google-user", "provider_config_key":"google-meet", "provider":"google-meet",
                    "errors":[], "end_user":{"id":"user", "email":"person@e3group.ai"}, "metadata":{}, "connection_config":{},
                    "created_at":"now", "updated_at":"now", "last_fetched_at":"now", "credentials":{}
                }))).mount(&server).await;
            Mock::given(method("POST")).and(path("/rest/v1/nango_connections"))
                .and(body_partial_json(json!({"user_id":"user", "integration_id":"google-meet", "connection_id":"e3-google-user", "status":"connected"})))
                .respond_with(ResponseTemplate::new(201)).mount(&server).await;
            let state = AppState::new(crate::NangoConfig::for_test(&server.uri(), &server.uri()));
            let auth = AuthContext {
                token: "session-token".to_string(),
                claims: serde_json::from_value(json!({"sub":"user"})).unwrap(),
            };
            let settings = GoogleSigninSettings {
                client_id: "signin-client".to_string(),
                client_secret: "signin-secret".to_string(),
                domain: "e3group.ai".to_string(),
                token_url: format!("{}/token", server.uri()),
                userinfo_url: format!("{}/userinfo", server.uri()),
            };
            let app = Router::new().route(
                "/google-signin",
                post(move |Json(body): Json<GoogleSigninRequest>| async move {
                    import_connection(state, auth, body, settings)
                        .await
                        .into_response()
                }),
            );
            let response = app
                .oneshot(
                    Request::post("/google-signin")
                        .header("Content-Type", "application/json")
                        .body(Body::from(
                            json!({"provider_refresh_token":"initial-refresh"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), expected);
            let requests = server.received_requests().await.unwrap();
            let imported = requests
                .iter()
                .any(|request| request.url.path() == "/connections");
            let persisted = requests.iter().any(|request| {
                request.url.path() == "/rest/v1/nango_connections"
                    && request.method.as_str() == "POST"
            });
            assert_eq!(imported, expected == 200);
            assert_eq!(persisted, expected == 200);
            if expected == 200 {
                let body: Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                        .unwrap();
                assert_eq!(
                    body,
                    json!({"connected":true, "connection_id":"e3-google-user"})
                );
            }
        }
    }
}
