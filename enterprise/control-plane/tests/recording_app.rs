use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anarlog_enterprise_control_plane::{
    recording_app::{self, RecordingAppState},
    recording_members::RecordingMemberAuth,
    shared_recordings::{Provider, SharedRecordingService},
};
use anlg_shared_recording::{encode_wav, sha256};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header as has_header, method, path},
};

const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWTFfCGljY6aw3Hrt\n\
kHmPRiazukxPLb6ilpRAewjW8nihRANCAATDskChT+Altkm9X7MI69T3IUmrQU0L\n\
950IxEzvw/x5BMEINRMrXLBJhqzO9Bm+d6JbqA21YQmd1Kt4RzLJR1W+\n\
-----END PRIVATE KEY-----";

fn token(user: &str) -> String {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("recording-test".into());
    encode(&header,&json!({"sub":user,"aud":"authenticated","exp":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()+3600,"user_metadata":{"role":"owner"}}),&EncodingKey::from_ec_pem(TEST_KEY.as_bytes()).unwrap()).unwrap()
}
fn request(method: &str, url: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(url)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn local_oauth_api_preflight_allows_only_the_isolated_preview_clients() {
    for invalid in [
        "http://shared-preview.example",
        "https://user:password@shared-preview.example",
        "https://shared-preview.example/path",
        "https://shared-preview.example/",
        "https://shared-preview.example?query=1",
        "https://shared-preview.example#fragment",
        "https://*.example",
        "*",
    ] {
        assert!(recording_app::local_preview_cors(Some(invalid)).is_err());
    }
    for web_origin in [None, Some("https://shared-preview.example")] {
        let app = axum::Router::new()
            .route("/nango/session", axum::routing::post(|| async {}))
            .layer(recording_app::local_preview_cors(web_origin).unwrap());
        for origin in [
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "tauri://localhost",
            "http://tauri.localhost",
            "https://shared-preview.example",
            "https://unrelated.example",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("OPTIONS")
                        .uri("/nango/session")
                        .header(header::ORIGIN, origin)
                        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                        .header(
                            header::ACCESS_CONTROL_REQUEST_HEADERS,
                            "authorization,content-type",
                        )
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let allowed = response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN);
            if !origin.starts_with("https://") || Some(origin) == web_origin {
                assert_eq!(allowed.unwrap(), origin);
            } else {
                assert!(allowed.is_none());
            }
            assert!(
                !response
                    .headers()
                    .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            );
        }
    }
}

#[tokio::test]
async fn signed_members_share_one_library_without_spoofing_uploader_or_retaining_revoked_access() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET")).and(path("/auth/v1/.well-known/jwks.json")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"keys":[{"kty":"EC","use":"sig","crv":"P-256","kid":"recording-test","x":"w7JAoU_gJbZJvV-zCOvU9yFJq0FNC_edCMRM78P8eQQ","y":"wQg1EytcsEmGrM70Gb53oluoDbVhCZ3Uq3hHMslHVb4","alg":"ES256"}]}))).mount(&upstream).await;
    let a = token("member-a");
    let b = token("member-b");
    let outsider = token("outsider");
    for (user, token) in [("member-a", &a), ("member-b", &b), ("outsider", &outsider)] {
        let rows = if user == "outsider" {
            json!([])
        } else {
            json!([{"user_id":user,"role":"member","workspace":{"id":"org","name":"Fixture organization","kind":"shared"}}])
        };
        Mock::given(method("GET"))
            .and(path("/rest/v1/workspace_memberships"))
            .and(has_header("authorization", format!("Bearer {token}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(rows))
            .with_priority(5)
            .mount(&upstream)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/rest/v1/nango_connections"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&upstream)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        SharedRecordingService::open(directory.path())
            .await
            .unwrap(),
    );
    service
        .automatic_admit(
            "org",
            "meeting",
            Provider::GoogleMeet,
            "conferenceRecords/provider-fixture",
            Some(chrono::Utc::now().timestamp_millis() - 100),
        )
        .await
        .unwrap();
    service
        .grant_actor_admission("org", "meeting", "member-a")
        .await
        .unwrap();
    let app = recording_app::router(RecordingAppState {
        service: service.clone(),
        members: RecordingMemberAuth::new(&upstream.uri(), "fixture-anon").unwrap(),
        identities: None,
        processor: None,
    })
    .merge(recording_app::provider_router(
        anlg_api_nango::NangoConfig {
            nango: serde_json::from_value(json!({
                "nango_api_base": upstream.uri(),
                "nango_api_key": "fixture-api-key",
                "nango_webhook_signing_key": "fixture-signing-key",
            }))
            .unwrap(),
            supabase_url: upstream.uri(),
            supabase_anon_key: "fixture-anon".into(),
            supabase_service_role_key: Some("fixture-service".into()),
        },
    ));
    assert_eq!(
        app.clone()
            .oneshot(request("POST", "/nango/session", "invalid", json!({})))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/nango/connections", &a, Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                "/v1/orgs/org/shared-recordings",
                &outsider,
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                "/v1/orgs/org/shared-recordings",
                "forged-jwt",
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/orgs/org/shared-recordings",
            &b,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let library: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(library["items"][0]["occurrenceId"], "meeting");
    let wav = encode_wav(&[100; 1600]).unwrap();
    let upload = json!({"metadata":{"uploaderId":"member-b","deviceId":"device-a","localCaptureId":"capture-a","segmentSequence":0,"localSessionId":"session-a","localTranscriptId":"transcript-a","sha256":sha256(&wav),"offsetFrames":0,"rank":99,"gaps":[],"timingBasis":"operator_verified"},"wavBase64":STANDARD.encode(&wav)});
    let source = "/v1/orgs/org/shared-recordings/meeting/captures/source-a";
    let response = app
        .clone()
        .oneshot(request("POST", source, &a, upload))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let manifest = service.manifest("org", "meeting").await.unwrap();
    assert_eq!(manifest.captures[0].metadata.uploader_id, "member-a");
    assert_eq!(manifest.captures[0].metadata.rank, 0);
    assert_eq!(
        app.clone()
            .oneshot(request("DELETE", source, &b, Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(request("DELETE", source, &a, Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    Mock::given(method("GET"))
        .and(path("/rest/v1/workspace_memberships"))
        .and(has_header("authorization", format!("Bearer {a}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .with_priority(1)
        .mount(&upstream)
        .await;
    assert_eq!(
        app.oneshot(request(
            "GET",
            "/v1/orgs/org/shared-recordings",
            &a,
            Value::Null
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::FORBIDDEN
    );
}
