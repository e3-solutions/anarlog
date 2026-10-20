use anlg_api_auth::AuthContext;
use anlg_nango::ListConnectionsParams;
use axum::{Extension, Json, extract::State};
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::Result;
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectionItem {
    pub integration_id: String,
    pub connection_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_identity: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListConnectionsResponse {
    pub connections: Vec<ConnectionItem>,
}

#[utoipa::path(
    get,
    path = "/connections",
    responses(
        (status = 200, description = "List of active connections", body = ListConnectionsResponse),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error"),
    ),
    tag = "nango",
)]
pub async fn list_connections(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<ListConnectionsResponse>> {
    let mut rows = state
        .supabase
        .list_user_connections(&auth.token, &auth.claims.sub)
        .await?;

    let nango_connections = state
        .nango
        .list_connections(ListConnectionsParams {
            end_user_id: Some(auth.claims.sub.clone()),
            ..Default::default()
        })
        .await
        .unwrap_or_default();

    let mut recovered = false;
    if state.supabase.is_configured() {
        for connection in &nango_connections {
            // A filtered list alone is not proof of ownership. Only Nango's
            // server-assigned end user can recover a missed auth webhook.
            if connection.end_user.as_ref().map(|user| user.id.as_str())
                != Some(auth.claims.sub.as_str())
                || !connection.errors.is_empty()
                || rows.iter().any(|row| {
                    row.integration_id == connection.provider_config_key
                        && row.connection_id == connection.connection_id
                })
            {
                continue;
            }
            state
                .supabase
                .upsert_connection(
                    &auth.claims.sub,
                    &connection.provider_config_key,
                    &connection.connection_id,
                    &connection.provider,
                )
                .await?;
            recovered = true;
        }
    }
    if recovered {
        rows = state
            .supabase
            .list_user_connections(&auth.token, &auth.claims.sub)
            .await?;
    }
    let nango_map: std::collections::HashMap<(&str, &str), _> = nango_connections
        .iter()
        .map(|connection| {
            (
                (
                    connection.provider_config_key.as_str(),
                    connection.connection_id.as_str(),
                ),
                connection,
            )
        })
        .collect();

    let mut connections: Vec<ConnectionItem> = rows
        .into_iter()
        .map(|row| {
            let nango = nango_map.get(&(row.integration_id.as_str(), row.connection_id.as_str()));
            let account_identity = nango.and_then(|connection| {
                super::identity::account_identity_from_tags(connection.tags.as_ref())
            });

            ConnectionItem {
                integration_id: row.integration_id,
                connection_id: row.connection_id,
                status: Some(row.status),
                last_error_type: row.last_error_type,
                last_error_description: row.last_error_description,
                last_error_at: row.last_error_at,
                updated_at: row.updated_at,
                account_identity,
            }
        })
        .collect();

    let missing: Vec<(String, String)> = connections
        .iter()
        .filter(|item| {
            item.account_identity.is_none() && item.status.as_deref() != Some("reconnect_required")
        })
        .map(|item| (item.integration_id.clone(), item.connection_id.clone()))
        .collect();

    if !missing.is_empty() {
        let resolved = futures_util::future::join_all(missing.into_iter().map(
            |(integration_id, connection_id)| {
                let nango = state.nango.clone();
                async move {
                    let identity = tokio::time::timeout(
                        std::time::Duration::from_secs(4),
                        super::identity::fetch_and_store_account_identity(
                            &nango,
                            &integration_id,
                            &connection_id,
                        ),
                    )
                    .await
                    .ok()
                    .flatten();
                    (connection_id, identity)
                }
            },
        ))
        .await;

        for (connection_id, identity) in resolved {
            if let Some(identity) = identity
                && let Some(item) = connections
                    .iter_mut()
                    .find(|item| item.connection_id == connection_id)
            {
                item.account_identity = Some(identity);
            }
        }
    }

    Ok(Json(ListConnectionsResponse { connections }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    #[tokio::test]
    async fn missed_webhook_recovers_only_connections_with_matching_server_end_user() {
        let nango = MockServer::start().await;
        let database = MockServer::start().await;
        let stored = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let rows = stored.clone();
        Mock::given(method("GET"))
            .and(path("/rest/v1/nango_connections"))
            .and(query_param("user_id", "eq.user-one"))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(rows.lock().unwrap().clone())
            })
            .mount(&database)
            .await;
        let rows = stored.clone();
        Mock::given(method("POST"))
            .and(path("/rest/v1/nango_connections"))
            .respond_with(move |request: &wiremock::Request| {
                let row: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                rows.lock().unwrap().push(row);
                ResponseTemplate::new(201)
            })
            .mount(&database)
            .await;
        let connection = |id: &str, owner: Option<&str>, errors: serde_json::Value| {
            serde_json::json!({
                "id":1,"connection_id":id,"provider":"google-meet",
                "provider_config_key":"google-meet","created":"2026-10-20T00:00:00Z",
                "metadata":null,"errors":errors,
                "end_user":owner.map(|id| serde_json::json!({"id":id})),
                "tags":{"end_user_id":"user-one","account_identity":"person@example.com"}
            })
        };
        Mock::given(method("GET"))
            .and(path("/connections"))
            .and(query_param("endUserId", "user-one"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "connections":[
                    connection("owned",Some("user-one"),serde_json::json!([])),
                    connection("foreign",Some("user-two"),serde_json::json!([])),
                    connection("unassigned",None,serde_json::json!([])),
                    connection("broken",Some("user-one"),serde_json::json!([{"type":"auth","log_id":"failure"}]))
                ]
            })))
            .mount(&nango).await;
        let state = AppState::new(crate::config::NangoConfig::for_test(
            &nango.uri(),
            &database.uri(),
        ));
        let auth = AuthContext {
            token: "verified-user-token".into(),
            claims: serde_json::from_value(serde_json::json!({"sub":"user-one"})).unwrap(),
        };
        let Json(response) = list_connections(State(state), Extension(auth))
            .await
            .unwrap();
        assert_eq!(response.connections.len(), 1);
        assert_eq!(response.connections[0].connection_id, "owned");
        let stored = stored.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0]["user_id"], "user-one");
    }
}
