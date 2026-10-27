use anlg_api_auth::{AuthContext, Claims};
use api_recordings::{
    application_health::{Snapshot, observe},
    recording_app::RecordingAppState,
    recording_members::{MemberError, RecordingMemberships},
    shared_recordings::SharedRecordingService,
};
use axum::{Extension, Json, extract::State, http::StatusCode};
use serde_json::{Value, json};
use std::sync::Arc;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn snapshot() -> Value {
    json!({"deviceId":"aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa","appVersion":"0.1.21","observedAt":chrono::Utc::now(),"permissions":{"microphone":"denied","systemAudio":"unknown","accessibility":"neverRequested"},"autoRecordDetected":false,"autoRecordScheduled":false,"recording":false,"uploadPendingCount":null,"uploadBlockedCount":null,"uploadError":null,"lastRecordingStartedAt":null,"lastRecordingStoppedAt":null,"lastRecordingDurationSeconds":null,"errorCategory":null})
}
fn auth() -> Extension<AuthContext> {
    Extension(AuthContext {
        token: "fixture-user-session".into(),
        claims: serde_json::from_value::<Claims>(json!({"sub":"user"})).unwrap(),
    })
}
async fn state(server: &MockServer, role: &str) -> RecordingAppState {
    Mock::given(method("GET")).and(path("/rest/v1/workspace_memberships"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"user_id":"user","role":role,"workspace":{"id":"org","name":"E3","kind":"shared"}}]))).mount(server).await;
    RecordingAppState {
        service: Arc::new(
            SharedRecordingService::supabase(&server.uri(), "fixture-service").unwrap(),
        ),
        members: RecordingMemberships::new(&server.uri(), "fixture-public-key").unwrap(),
        identities: None,
        processor: None,
    }
}
#[tokio::test]
async fn health_ingestion_binds_user_session_and_rejects_identity_or_content_fields() {
    let server = MockServer::start().await;
    let state = state(&server, "member").await;
    Mock::given(method("POST"))
        .and(path("/rest/v1/rpc/application_health_observe"))
        .and(header("authorization", "Bearer fixture-user-session"))
        .and(header("apikey", "fixture-public-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Value::Null))
        .mount(&server)
        .await;
    for permission in ["denied", "unknown", "neverRequested", "authorized"] {
        let mut value = snapshot();
        value["permissions"]["microphone"] = json!(permission);
        assert_eq!(
            observe(
                State(state.clone()),
                auth(),
                Json(serde_json::from_value(value).unwrap())
            )
            .await
            .unwrap(),
            StatusCode::NO_CONTENT
        );
    }
    for field in [
        "userId",
        "workspaceId",
        "googleConnected",
        "transcript",
        "error",
    ] {
        let mut value = snapshot();
        value[field] = json!("private payload");
        assert!(serde_json::from_value::<Snapshot>(value).is_err());
    }
    let mut value = snapshot();
    value["lastRecordingDurationSeconds"] = json!(86401);
    assert!(matches!(
        observe(
            State(state),
            auth(),
            Json(serde_json::from_value(value).unwrap())
        )
        .await,
        Err(MemberError::Invalid(_))
    ));
    let requests = server.received_requests().await.unwrap();
    for request in requests.iter().filter(|r| r.method.as_str() == "POST") {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["p_snapshot"]["permissions"]["systemAudio"], "unknown");
        assert!(body["p_snapshot"].get("userId").is_none());
    }
}
#[tokio::test]
async fn health_collection_route_requires_authentication() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let server = MockServer::start().await;
    let router = api_recordings::recording_app::router(
        state(&server, "owner").await,
        anlg_api_auth::AuthState::new(&server.uri()),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/application-health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
