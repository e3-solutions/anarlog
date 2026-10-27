use anlg_api_recordings::recording_members::RecordingMemberships;
use axum::{
    body::{Body, to_bytes},
    extract::{OriginalUri, Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use anlg_api_auth::AuthContext;
pub use anlg_api_auth::{AuthState, require_auth};

const DEVICE_FINGERPRINT_HEADER: &str = "x-device-fingerprint";

pub async fn sentry_and_analytics(mut request: Request, next: Next) -> Response {
    let device_fingerprint = request
        .headers()
        .get(DEVICE_FINGERPRINT_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(String::from);

    if let Some(auth) = request.extensions().get::<AuthContext>() {
        let user_id = auth.claims.sub.clone();
        request
            .extensions_mut()
            .insert(anlg_analytics::AuthenticatedUserId(user_id));
    }

    if let Some(fingerprint) = device_fingerprint {
        request
            .extensions_mut()
            .insert(anlg_analytics::DeviceFingerprint(fingerprint));
    }

    next.run(request).await
}

pub async fn require_integration_access(
    State(members): State<Option<RecordingMemberships>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(auth) = request.extensions().get::<AuthContext>().cloned() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if crate::PAID_ENTITLEMENTS
        .iter()
        .any(|value| auth.claims.has_entitlement(value))
    {
        return next.run(request).await;
    }
    let path = request
        .extensions()
        .get::<OriginalUri>()
        .map_or(request.uri().path(), |original| original.0.path())
        .to_owned();
    let allowed = if path == "/nango/session" && request.method() == Method::POST {
        let body = match to_bytes(
            std::mem::replace(request.body_mut(), Body::empty()),
            64 * 1024,
        )
        .await
        {
            Ok(body) => body,
            Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        };
        let integration = serde_json::from_slice::<serde_json::Value>(&body).ok();
        let allowed = matches!(
            integration
                .as_ref()
                .and_then(|value| value.get("integration_id"))
                .and_then(|value| value.as_str()),
            Some("google-meet" | "slack" | "google-calendar")
        );
        *request.body_mut() = Body::from(body);
        allowed
    } else {
        path.starts_with("/calendar/google/")
            || (path == "/nango/google-signin" && request.method() == Method::POST)
    };
    if allowed && let Some(members) = members {
        let memberships = match members.memberships(&auth).await {
            Ok(memberships) => memberships,
            Err(error) => return error.into_response(),
        };
        match members.policy(&auth, &memberships).await {
            Ok(Some(_)) => return next.run(request).await,
            Ok(None) => {}
            Err(error) => return error.into_response(),
        }
    }
    (StatusCode::FORBIDDEN, "subscription_required").into_response()
}

#[cfg(test)]
mod integration_access_tests {
    use super::*;
    use axum::{Extension, Router, routing::post};
    use serde_json::json;
    use tower::ServiceExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn e3_integrations_require_live_domain_membership_and_preserve_unrelated_paid_gates() {
        const WORKSPACE: &str = "0af47883-c9ff-4a84-9b00-66bd7c5b03c6";
        for (email, member, route, integration, paid, expected) in [
            (
                "user@e3group.ai",
                true,
                "/nango/google-signin",
                "",
                false,
                200,
            ),
            (
                "user@outside.test",
                true,
                "/nango/google-signin",
                "",
                false,
                403,
            ),
            (
                "user@e3group.ai",
                true,
                "/nango/session",
                "google-meet",
                false,
                200,
            ),
            (
                "user@e3group.ai",
                true,
                "/nango/session",
                "slack",
                false,
                200,
            ),
            (
                "user@e3group.ai",
                true,
                "/nango/session",
                "google-calendar",
                false,
                200,
            ),
            (
                "user@e3group.ai",
                true,
                "/calendar/google/list-calendars",
                "",
                false,
                200,
            ),
            (
                "user@outside.test",
                true,
                "/nango/session",
                "google-meet",
                false,
                403,
            ),
            (
                "user@e3group.ai",
                false,
                "/nango/session",
                "slack",
                false,
                403,
            ),
            (
                "user@e3group.ai",
                true,
                "/nango/session",
                "google-drive",
                false,
                403,
            ),
            (
                "user@e3group.ai",
                true,
                "/calendar/outlook/list-calendars",
                "",
                false,
                403,
            ),
            (
                "user@outside.test",
                false,
                "/nango/session",
                "google-drive",
                true,
                200,
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET")).and(path("/auth/v1/user"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id":"user", "email":email, "email_confirmed_at":"2026-10-27T00:00:00Z", "is_anonymous":false
                }))).mount(&server).await;
            Mock::given(method("GET")).and(path("/rest/v1/workspace_memberships"))
                .respond_with(ResponseTemplate::new(200).set_body_json(if member {
                    json!([{"user_id":"user","role":"member","workspace":{"id":WORKSPACE,"name":"E3","kind":"shared"}}])
                } else {json!([])})).mount(&server).await;
            let members = RecordingMemberships::new(&server.uri(), "anon")
                .unwrap()
                .with_domain_enrollment("e3group.ai", WORKSPACE, "service")
                .unwrap();
            let auth = AuthContext { token:"user-token".into(), claims:serde_json::from_value(json!({
                "sub":"user", "email":"untrusted@e3group.ai", "entitlements": if paid {vec!["hyprnote_pro"]} else {vec![]}
            })).unwrap() };
            let app = Router::new()
                .route(route, post(|body: String| async move { body }))
                .route_layer(axum::middleware::from_fn_with_state(
                    Some(members),
                    require_integration_access,
                ))
                .layer(Extension(auth));
            let payload = json!({"integration_id":integration,"mode":"connect"}).to_string();
            let response = app
                .oneshot(
                    Request::post(route)
                        .body(Body::from(payload.clone()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status().as_u16(),
                expected,
                "{email}, {member}, {route}, {integration}, paid={paid}"
            );
            if expected == 200 {
                assert_eq!(
                    to_bytes(response.into_body(), 65536)
                        .await
                        .unwrap()
                        .as_ref(),
                    payload.as_bytes()
                );
            }
        }
    }
}
