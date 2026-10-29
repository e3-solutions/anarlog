mod common;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use api_recordings::{
    recording_app::{self, RecordingAppState},
    recording_members::RecordingMemberships,
    shared_recordings::Provider,
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header as has_header, method, path},
};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn encode_wav(samples: &[i16]) -> Result<Vec<u8>, hound::Error> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut buffer,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(buffer.into_inner())
}

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
    let (_storage, service) = common::service("org").await;
    let service = Arc::new(service);
    service
        .admit_actor(
            "org",
            "meeting",
            "member-a",
            Provider::GoogleMeet,
            "conferenceRecords/provider-fixture",
            Some(chrono::Utc::now().timestamp_millis() - 100),
        )
        .await
        .unwrap();
    let app = recording_app::router(
        RecordingAppState {
            service: service.clone(),
            members: RecordingMemberships::new(&upstream.uri(), "fixture-anon").unwrap(),
            identities: None,
            processor: None,
        },
        anlg_api_auth::AuthState::new(&upstream.uri()),
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                "/v1/shared-recordings/me",
                "invalid",
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/orgs/org/shared-recordings/admit")
                .header(header::CONTENT_LENGTH, (140 * 1024 * 1024 + 1).to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let large_json = json!({"padding": "x".repeat(2 * 1024 * 1024 + 1)});
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                "/v1/orgs/org/shared-recordings/admit",
                &a,
                large_json
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
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
            .oneshot(request(
                "GET",
                "/v1/orgs/org/shared-recordings/meeting/transcript",
                &a,
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let original = format!("{source}/audio");
    for reader in [&a, &b] {
        let response = app
            .clone()
            .oneshot(request("GET", &original, reader, Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");
        assert_eq!(
            to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .as_ref(),
            wav
        );
    }
    assert_eq!(
        app.clone()
            .oneshot(request("GET", &original, &outsider, Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
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
    let fallback = "/v1/orgs/org/shared-recordings/admit-unverified";
    assert_eq!(
        app.clone()
            .oneshot(request(
                "POST",
                fallback,
                &a,
                json!({"localCaptureId":"unassigned-capture","uploaderId":"member-b"})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut receipts = Vec::new();
    for actor in [&a, &a, &b] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                fallback,
                actor,
                json!({"localCaptureId":"unassigned-capture"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let receipt: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(receipt["provider"], "unverified");
        receipts.push(receipt["occurrenceId"].as_str().unwrap().to_string());
    }
    assert_eq!(receipts[0], receipts[1]);
    assert_ne!(receipts[0], receipts[2]);
    for (url, expected) in [
        ("/v1/orgs/org/shared-recordings", 1),
        ("/v1/orgs/org/shared-recordings?includeUnverified=true", 2),
    ] {
        let response = app
            .clone()
            .oneshot(request("GET", url, &a, Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let list: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(list["items"].as_array().unwrap().len(), expected);
        assert!(
            !list["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["occurrenceId"] == receipts[2])
        );
    }
    let unassigned_path = format!("/v1/orgs/org/shared-recordings/{}", receipts[0]);
    assert_eq!(
        app.clone()
            .oneshot(request("GET", &unassigned_path, &b, Value::Null))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let unassigned_source = format!("{unassigned_path}/captures/unassigned-source");
    let upload = json!({"metadata":{"uploaderId":"member-b","deviceId":"device-a","localCaptureId":"unassigned-capture","segmentSequence":0,"localSessionId":"session-a","localTranscriptId":"transcript-a","sha256":sha256(&wav),"offsetFrames":0,"rank":0,"gaps":[{"start":0,"end":100}],"timingBasis":"device_clock_hint","audioTimelineVerified":false},"wavBase64":STANDARD.encode(&wav)});
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &unassigned_source, &b, upload.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.clone()
            .oneshot(request("POST", &unassigned_source, &a, upload))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let recovered = service.manifest("org", &receipts[0]).await.unwrap();
    assert_eq!(recovered.captures[0].metadata.uploader_id, "member-a");
    assert_eq!(
        recovered.captures[0].metadata.audio_timeline_verified,
        Some(false)
    );
    assert!(recovered.captures[0].metadata.gaps.is_empty());
    assert_eq!(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("{unassigned_source}/audio"),
                &b,
                Value::Null
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
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
