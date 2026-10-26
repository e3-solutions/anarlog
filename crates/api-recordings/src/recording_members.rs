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
    #[error("recording identity or revision conflict")]
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
        Ok(Self {
            client,
            url: supabase_url.trim_end_matches('/').into(),
            anon_key: anon_key.into(),
        })
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
                ("integration_id", "in.(google-meet,slack)"),
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
