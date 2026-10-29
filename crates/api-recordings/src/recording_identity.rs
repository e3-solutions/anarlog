use anlg_api_auth::AuthContext;
use anlg_api_nango::{GoogleMeet, NangoConnectionState, NangoIntegrationId, Slack};
use anlg_http::HttpClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{recording_members::MemberError, shared_recordings::Provider};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionRequest {
    pub provider: Provider,
    pub room_identity: Option<String>,
    pub connection_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedOccurrence {
    pub occurrence_id: String,
    pub provider: Provider,
    pub provider_occurrence_id: String,
    pub provider_started_at_ms: Option<i64>,
    pub server_observed_at_ms: i64,
}

#[derive(Clone)]
pub struct RecordingIdentityResolver {
    connections: NangoConnectionState,
}

impl RecordingIdentityResolver {
    pub fn new(connections: NangoConnectionState) -> Self {
        Self { connections }
    }

    pub async fn google_calendar_access(&self, auth: &AuthContext, connection_id: &str) -> bool {
        self.connections
            .google_meet_calendar_access(&auth.token, &auth.claims.sub, connection_id)
            .await
    }

    pub async fn resolve(
        &self,
        auth: &AuthContext,
        request: &AdmissionRequest,
    ) -> Result<ResolvedOccurrence, MemberError> {
        if request.connection_id.is_empty() || request.connection_id.len() > 256 {
            return Err(MemberError::Invalid(
                "provider connection is required".into(),
            ));
        }
        let integration = match request.provider {
            Provider::GoogleMeet => GoogleMeet::ID,
            Provider::SlackHuddle => Slack::ID,
            Provider::Unverified => return Err(MemberError::Unresolved),
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let http = self
                .connections
                .build_http_client(
                    &auth.token,
                    &auth.claims.sub,
                    integration,
                    &request.connection_id,
                )
                .await
                .map_err(|_| MemberError::Unresolved)?;
            match request.provider {
                Provider::Unverified => Err(MemberError::Unresolved),
                Provider::GoogleMeet => {
                    let code = meet_code(
                        request
                            .room_identity
                            .as_deref()
                            .ok_or(MemberError::Unresolved)?,
                    )?;
                    let verified = anlg_api_nango::resolve_verified_active_conference(
                        http.into_proxy().retries(0),
                        &code,
                    )
                    .await
                    .map_err(|_| MemberError::Unresolved)?;
                    let start = chrono::DateTime::parse_from_rfc3339(&verified.start_time)
                        .map_err(|_| MemberError::Unresolved)?
                        .timestamp_millis();
                    if start > chrono::Utc::now().timestamp_millis() + 5000 {
                        return Err(MemberError::Unresolved);
                    }
                    Ok(resolved(
                        Provider::GoogleMeet,
                        verified.conference_record,
                        Some(start),
                    ))
                }
                Provider::SlackHuddle => {
                    resolve_slack(&http.with_base_url_override("https://slack.com")).await
                }
            }
        })
        .await
        .map_err(|_| MemberError::Upstream)?
    }
}

async fn resolve_slack<C: HttpClient>(http: &C) -> Result<ResolvedOccurrence, MemberError> {
    let auth = get_json(http, "/api/auth.test").await?;
    slack_ok(&auth)?;
    if auth
        .get("bot_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return Err(MemberError::Unresolved);
    }
    let user = auth
        .get("user_id")
        .and_then(Value::as_str)
        .filter(|id| slack_id(id, b'U'))
        .ok_or(MemberError::Unresolved)?;
    let team = auth
        .get("team_id")
        .and_then(Value::as_str)
        .filter(|id| slack_id(id, b'T'))
        .ok_or(MemberError::Unresolved)?;
    let info = get_json(http, &format!("/api/users.info?user={user}")).await?;
    slack_ok(&info)?;
    if info.pointer("/user/id").and_then(Value::as_str) != Some(user)
        || info.pointer("/user/is_bot").and_then(Value::as_bool) == Some(true)
        || info
            .pointer("/user/profile/huddle_state")
            .and_then(Value::as_str)
            != Some("in_a_huddle")
    {
        return Err(MemberError::Unresolved);
    }
    let call = info
        .pointer("/user/profile/huddle_state_call_id")
        .and_then(Value::as_str)
        .filter(|id| slack_id(id, b'R'))
        .ok_or(MemberError::Unresolved)?;
    Ok(resolved(
        Provider::SlackHuddle,
        format!("{team}/{call}"),
        None,
    ))
}

fn resolved(
    provider: Provider,
    provider_occurrence_id: String,
    provider_started_at_ms: Option<i64>,
) -> ResolvedOccurrence {
    let provider_name = match provider {
        Provider::GoogleMeet => "google_meet",
        Provider::SlackHuddle => "slack_huddle",
        Provider::Unverified => "unverified",
    };
    let occurrence_id = format!(
        "meeting-{}",
        crate::shared_recordings::hex_sha(
            format!("{provider_name}:{provider_occurrence_id}").as_bytes()
        )
    );
    ResolvedOccurrence {
        occurrence_id,
        provider,
        provider_occurrence_id,
        provider_started_at_ms,
        server_observed_at_ms: chrono::Utc::now().timestamp_millis(),
    }
}

fn slack_id(id: &str, prefix: u8) -> bool {
    id.len() > 1
        && id.len() <= 64
        && id.as_bytes()[0] == prefix
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}
fn slack_ok(value: &Value) -> Result<(), MemberError> {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(MemberError::Unresolved);
    }
    Ok(())
}

fn meet_code(identity: &str) -> Result<String, MemberError> {
    let room = identity
        .split(":tab:")
        .next()
        .unwrap_or(identity)
        .split(":window:")
        .next()
        .unwrap_or(identity);
    let url = url::Url::parse(room).map_err(|_| MemberError::Unresolved)?;
    if url.scheme() != "https"
        || url.host_str() != Some("meet.google.com")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(MemberError::Unresolved);
    }
    let code = url.path().trim_matches('/');
    if code.len() != 12
        || !code.bytes().enumerate().all(|(index, byte)| {
            if index == 3 || index == 8 {
                byte == b'-'
            } else {
                byte.is_ascii_lowercase()
            }
        })
    {
        return Err(MemberError::Unresolved);
    }
    Ok(code.to_owned())
}

async fn get_json<C: HttpClient>(http: &C, path: &str) -> Result<Value, MemberError> {
    let bytes = http.get(path).await.map_err(|_| MemberError::Unresolved)?;
    if bytes.len() > 1024 * 1024 {
        return Err(MemberError::Upstream);
    }
    serde_json::from_slice(&bytes).map_err(|_| MemberError::Upstream)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::collections::HashMap<String, Value>);
    impl HttpClient for Fixture {
        async fn get(&self, path: &str) -> Result<Vec<u8>, anlg_http::Error> {
            Ok(serde_json::to_vec(
                self.0.get(path).ok_or("unknown provider request")?,
            )?)
        }
        async fn post(&self, _: &str, _: Vec<u8>, _: &str) -> Result<Vec<u8>, anlg_http::Error> {
            Err("unused".into())
        }
        async fn put(&self, _: &str, _: Vec<u8>) -> Result<Vec<u8>, anlg_http::Error> {
            Err("unused".into())
        }
        async fn patch(&self, _: &str, _: Vec<u8>) -> Result<Vec<u8>, anlg_http::Error> {
            Err("unused".into())
        }
        async fn delete(&self, _: &str) -> Result<Vec<u8>, anlg_http::Error> {
            Err("unused".into())
        }
    }
    #[tokio::test]
    async fn huddle_identity_separates_calls_in_one_channel_and_rejects_inactive_or_bot_accounts() {
        let mut provider = Fixture(std::collections::HashMap::from([
            (
                "/api/auth.test".into(),
                serde_json::json!({"ok":true,"user_id":"U123","team_id":"T123"}),
            ),
            (
                "/api/users.info?user=U123".into(),
                serde_json::json!({"ok":true,"user":{"id":"U123","profile":{"huddle_state":"in_a_huddle","huddle_state_call_id":"R123","huddle_state_channel_id":"C123"}}}),
            ),
        ]));
        let first = resolve_slack(&provider).await.unwrap();
        assert_eq!(first.provider_occurrence_id, "T123/R123");
        provider.0.get_mut("/api/users.info?user=U123").unwrap()["user"]["profile"]["huddle_state_call_id"] =
            "R456".into();
        assert_ne!(
            first.occurrence_id,
            resolve_slack(&provider).await.unwrap().occurrence_id
        );
        provider.0.get_mut("/api/users.info?user=U123").unwrap()["user"]["profile"]["huddle_state"] =
            "default".into();
        assert!(matches!(
            resolve_slack(&provider).await,
            Err(MemberError::Unresolved)
        ));
        provider.0.get_mut("/api/auth.test").unwrap()["bot_id"] = "B123".into();
        assert!(matches!(
            resolve_slack(&provider).await,
            Err(MemberError::Unresolved)
        ));
    }
}
