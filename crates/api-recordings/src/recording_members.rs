use std::{sync::Arc, time::Duration};

use anlg_api_auth::AuthContext;
use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum MemberError {
    #[error("a valid app session is required")]
    Unauthorized,
    #[error("active organization membership is required")]
    Forbidden,
    #[error("organization or provider service unavailable")]
    Upstream,
    #[error("{0}")]
    Invalid(String),
    #[error("provider identity is unresolved; capture remains local")]
    Unresolved,
    #[error("recording identity conflict")]
    Conflict,
    #[error("recording not found")]
    NotFound,
}

impl IntoResponse for MemberError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Unresolved | Self::Conflict => StatusCode::CONFLICT,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Upstream => StatusCode::BAD_GATEWAY,
        };
        (
            status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({"error":self.to_string()})),
        )
            .into_response()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationMembership {
    pub id: String,
    pub name: String,
    pub role: String,
}

#[derive(Clone)]
pub struct RecordingMemberships {
    client: reqwest::Client,
    url: Arc<str>,
    anon_key: Arc<str>,
    enrollment: Option<DomainEnrollment>,
}

#[derive(Clone)]
struct DomainEnrollment {
    domain: String,
    workspace_id: String,
    service_key: Arc<str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingPolicy {
    workspace_id: String,
    auto_record_meetings: bool,
    auto_share_recordings: bool,
}

impl RecordingMemberships {
    pub fn new(supabase_url: &str, anon_key: &str) -> Result<Self, MemberError> {
        let url = url::Url::parse(supabase_url)
            .map_err(|_| MemberError::Invalid("invalid Supabase URL".into()))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || anon_key.is_empty()
            || (url.scheme() == "http"
                && !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
        {
            return Err(MemberError::Invalid(
                "invalid Supabase authentication configuration".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| MemberError::Upstream)?;
        let mut members = Self {
            client,
            url: supabase_url.trim_end_matches('/').into(),
            anon_key: anon_key.into(),
            enrollment: None,
        };
        let domain = std::env::var("E3_RECORDING_ENROLLMENT_DOMAIN").ok();
        let workspace = std::env::var("E3_RECORDING_ENROLLMENT_WORKSPACE_ID").ok();
        if domain.is_some() || workspace.is_some() {
            members = members.with_domain_enrollment(
                domain.as_deref().unwrap_or_default(),
                workspace.as_deref().unwrap_or_default(),
                &std::env::var("SUPABASE_SERVICE_ROLE_KEY").unwrap_or_default(),
            )?;
        }
        Ok(members)
    }

    pub fn with_domain_enrollment(
        mut self,
        domain: &str,
        workspace_id: &str,
        service_key: &str,
    ) -> Result<Self, MemberError> {
        let domain = domain.trim().to_ascii_lowercase();
        if !domain.contains('.')
            || domain.split('.').any(|label| {
                label.is_empty()
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || workspace_id.len() != 36
            || !workspace_id.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
            || service_key.trim().is_empty()
        {
            return Err(MemberError::Invalid(
                "invalid recording domain enrollment configuration".into(),
            ));
        }
        self.enrollment = Some(DomainEnrollment {
            domain,
            workspace_id: workspace_id.into(),
            service_key: service_key.into(),
        });
        Ok(self)
    }

    async fn verified_domain_user(&self, auth: &AuthContext) -> Result<bool, MemberError> {
        let Some(config) = &self.enrollment else {
            return Ok(false);
        };
        #[derive(Deserialize)]
        struct User {
            id: String,
            email: Option<String>,
            email_confirmed_at: Option<String>,
            is_anonymous: bool,
        }
        let user: User = json_response(
            self.client
                .get(format!("{}/auth/v1/user", self.url))
                .header("apikey", self.anon_key.as_ref())
                .bearer_auth(&auth.token),
        )
        .await?;
        Ok(user.id == auth.claims.sub
            && !user.is_anonymous
            && user
                .email_confirmed_at
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            && user
                .email
                .as_deref()
                .and_then(|email| email.rsplit_once('@'))
                .is_some_and(|(local, domain)| {
                    !local.is_empty()
                        && !local.contains('@')
                        && domain.eq_ignore_ascii_case(&config.domain)
                }))
    }

    pub async fn policy(
        &self,
        auth: &AuthContext,
        memberships: &[OrganizationMembership],
    ) -> Result<Option<RecordingPolicy>, MemberError> {
        let Some(config) = &self.enrollment else {
            return Ok(None);
        };
        if !memberships
            .iter()
            .any(|membership| membership.id == config.workspace_id)
            || !self.verified_domain_user(auth).await?
        {
            return Ok(None);
        }
        Ok(Some(RecordingPolicy {
            workspace_id: config.workspace_id.clone(),
            auto_record_meetings: true,
            auto_share_recordings: true,
        }))
    }

    pub async fn enroll(&self, auth: &AuthContext) -> Result<(), MemberError> {
        let config = self.enrollment.as_ref().ok_or(MemberError::NotFound)?;
        if !self.verified_domain_user(auth).await? {
            return Err(MemberError::Forbidden);
        }
        let admin = |method, path: &str| {
            self.client
                .request(method, format!("{}{path}", self.url))
                .header("apikey", config.service_key.as_ref())
                .bearer_auth(config.service_key.as_ref())
        };
        let workspaces: Vec<serde_json::Value> =
            json_response(admin(reqwest::Method::GET, "/rest/v1/workspaces").query(&[
                ("select", "id"),
                ("id", &format!("eq.{}", config.workspace_id)),
                ("kind", "eq.shared"),
                ("deleted_at", "is.null"),
                ("limit", "1"),
            ]))
            .await?;
        if workspaces.len() != 1 {
            return Err(MemberError::Forbidden);
        }
        let rows: Vec<serde_json::Value> = json_response(
            admin(reqwest::Method::GET, "/rest/v1/workspace_memberships").query(&[
                ("select", "user_id"),
                ("workspace_id", &format!("eq.{}", config.workspace_id)),
                ("user_id", &format!("eq.{}", auth.claims.sub)),
                ("limit", "1"),
            ]),
        )
        .await?;
        if !rows.is_empty() {
            return Ok(());
        }
        // Ignore a concurrent insert rather than restoring a revoked membership or changing a role.
        let response = admin(reqwest::Method::POST, "/rest/v1/workspace_memberships")
            .query(&[("on_conflict", "workspace_id,user_id")])
            .header("prefer", "resolution=ignore-duplicates,return=minimal")
            .json(&serde_json::json!({"workspace_id":config.workspace_id,"user_id":auth.claims.sub,"role":"member"}))
            .send().await.map_err(|_| MemberError::Upstream)?;
        if !response.status().is_success() {
            return Err(MemberError::Upstream);
        }
        Ok(())
    }

    pub async fn authorize(
        &self,
        auth: &AuthContext,
        workspace: &str,
    ) -> Result<OrganizationMembership, MemberError> {
        self.memberships(auth)
            .await?
            .into_iter()
            .find(|membership| membership.id == workspace)
            .ok_or(MemberError::Forbidden)
    }

    pub async fn memberships(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<OrganizationMembership>, MemberError> {
        #[derive(Deserialize)]
        struct Workspace {
            id: String,
            name: String,
            kind: String,
        }
        #[derive(Deserialize)]
        struct Row {
            user_id: String,
            role: String,
            workspace: Workspace,
        }
        let filter = format!("eq.{}", auth.claims.sub);
        let request = self
            .client
            .get(format!("{}/rest/v1/workspace_memberships", self.url))
            .header("apikey", self.anon_key.as_ref())
            .bearer_auth(&auth.token)
            .query(&[
                (
                    "select",
                    "user_id,role,workspace:workspaces!inner(id,name,kind)",
                ),
                ("user_id", filter.as_str()),
                ("deleted_at", "is.null"),
                ("workspace.deleted_at", "is.null"),
                ("workspace.kind", "eq.shared"),
                ("limit", "129"),
            ]);
        let rows: Vec<Row> = json_response(request).await?;
        if rows.len() > 128 {
            return Err(MemberError::Upstream);
        }
        rows.into_iter()
            .map(|row| {
                if row.user_id != auth.claims.sub
                    || row.workspace.kind != "shared"
                    || !matches!(row.role.as_str(), "owner" | "admin" | "member")
                {
                    return Err(MemberError::Forbidden);
                }
                Ok(OrganizationMembership {
                    id: row.workspace.id,
                    name: row.workspace.name,
                    role: row.role,
                })
            })
            .collect()
    }

    pub(crate) async fn observe_health(
        &self,
        auth: &AuthContext,
        snapshot: serde_json::Value,
    ) -> Result<(), MemberError> {
        let _: serde_json::Value = json_response(
            self.client
                .post(format!(
                    "{}/rest/v1/rpc/application_health_observe",
                    self.url
                ))
                .header("apikey", self.anon_key.as_ref())
                .bearer_auth(&auth.token)
                .json(&serde_json::json!({"p_snapshot": snapshot})),
        )
        .await?;
        Ok(())
    }

    pub async fn connections(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<ProviderConnection>, MemberError> {
        let filter = format!("eq.{}", auth.claims.sub);
        let request = self
            .client
            .get(format!("{}/rest/v1/nango_connections", self.url))
            .header("apikey", self.anon_key.as_ref())
            .bearer_auth(&auth.token)
            .query(&[
                ("select", "integration_id,connection_id,status"),
                ("user_id", filter.as_str()),
                ("integration_id", "in.(google-meet,google-calendar,slack)"),
                ("limit", "129"),
            ]);
        let rows: Vec<ProviderConnection> = json_response(request).await?;
        if rows.len() > 128 {
            return Err(MemberError::Upstream);
        }
        Ok(rows)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase"))]
pub struct ProviderConnection {
    pub integration_id: String,
    pub connection_id: String,
    pub status: String,
}

async fn json_response<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, MemberError> {
    let mut response = request.send().await.map_err(|_| MemberError::Upstream)?;
    if response.status() == StatusCode::UNAUTHORIZED {
        return Err(MemberError::Unauthorized);
    }
    if response.status() == StatusCode::FORBIDDEN {
        return Err(MemberError::Forbidden);
    }
    if response.status() == StatusCode::BAD_REQUEST {
        return Err(MemberError::Invalid("invalid request".into()));
    }
    if !response.status().is_success() {
        return Err(MemberError::Upstream);
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(MemberError::Upstream);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| MemberError::Upstream)? {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(MemberError::Upstream);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| MemberError::Upstream)
}

#[cfg(test)]
mod enrollment_tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header as has_header, headers, method, path, query_param},
    };

    const WORKSPACE: &str = "ffd5094d-4ae2-4d81-a96d-613287177e82";

    fn auth() -> AuthContext {
        AuthContext {
            token: "user-token".into(),
            claims: serde_json::from_value(json!({"sub":"user","email":"spoofed@e3group.ai"}))
                .unwrap(),
        }
    }

    fn user() -> serde_json::Value {
        json!({"id":"user","email":"person@E3GROUP.AI","email_confirmed_at":"2026-10-27T00:00:00Z","is_anonymous":false})
    }

    fn members(server: &MockServer) -> RecordingMemberships {
        RecordingMemberships {
            client: reqwest::Client::new(),
            url: server.uri().into(),
            anon_key: "anon".into(),
            enrollment: None,
        }
        .with_domain_enrollment("e3group.ai", WORKSPACE, "service")
        .unwrap()
    }

    #[tokio::test]
    async fn enrollment_requires_verified_exact_domain_identity_not_claims_or_metadata() {
        for (field, value) in [
            ("id", json!("other")),
            ("email", json!("person@coreedgesolution.com")),
            ("email", json!("person@e3group.ai.attacker.test")),
            ("email", json!("person@sub.e3group.ai")),
            ("email_confirmed_at", serde_json::Value::Null),
            ("is_anonymous", json!(true)),
        ] {
            let server = MockServer::start().await;
            let mut response = user();
            response[field] = value;
            response["user_metadata"] = json!({"email":"person@e3group.ai","email_verified":true});
            Mock::given(method("GET"))
                .and(path("/auth/v1/user"))
                .and(has_header("authorization", "Bearer user-token"))
                .and(has_header("apikey", "anon"))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .expect(1)
                .mount(&server)
                .await;
            assert!(matches!(
                members(&server).enroll(&auth()).await,
                Err(MemberError::Forbidden)
            ));
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn enrollment_inserts_only_new_members_and_never_restores_or_overwrites_memberships() {
        for existing in [
            serde_json::Value::Null,
            json!({"user_id":"user","role":"owner","deleted_at":null}),
            json!({"user_id":"user","role":"member","deleted_at":"2026-10-26T00:00:00Z"}),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/auth/v1/user"))
                .respond_with(ResponseTemplate::new(200).set_body_json(user()))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/rest/v1/workspaces"))
                .and(has_header("authorization", "Bearer service"))
                .and(query_param("id", format!("eq.{WORKSPACE}")))
                .and(query_param("kind", "eq.shared"))
                .and(query_param("deleted_at", "is.null"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"id":WORKSPACE}])))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/rest/v1/workspace_memberships"))
                .and(has_header("authorization", "Bearer service"))
                .and(query_param("workspace_id", format!("eq.{WORKSPACE}")))
                .and(query_param("user_id", "eq.user"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(if existing.is_null() {
                        json!([])
                    } else {
                        json!([existing])
                    }),
                )
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/rest/v1/workspace_memberships"))
                .and(has_header("authorization", "Bearer service"))
                .and(headers(
                    "prefer",
                    vec!["resolution=ignore-duplicates", "return=minimal"],
                ))
                .and(query_param("on_conflict", "workspace_id,user_id"))
                .respond_with(ResponseTemplate::new(201))
                .expect(if existing.is_null() { 1 } else { 0 })
                .mount(&server)
                .await;
            let memberships = members(&server);
            memberships.enroll(&auth()).await.unwrap();
            assert!(memberships.policy(&auth(), &[]).await.unwrap().is_none());
            let policy = memberships
                .policy(
                    &auth(),
                    &[OrganizationMembership {
                        id: WORKSPACE.into(),
                        name: "E3".into(),
                        role: "member".into(),
                    }],
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(policy.workspace_id, WORKSPACE);
            assert!(policy.auto_record_meetings && policy.auto_share_recordings);
            for request in server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.method.as_str() == "POST")
            {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
                    json!({"workspace_id":WORKSPACE,"user_id":"user","role":"member"})
                );
            }
        }
    }
}
