use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anarlog_enterprise_control_plane::{
    auth::StaticTokenAuthenticator,
    recording_transcription::{self, RecordingProcessor, RecordingTranscriber},
    shared_recordings::{self, Revision, SharedRecordingService},
};
use anlg_shared_recording::{encode_wav, sha256};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn start(router: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { base, task }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture_provider(State(fail): State<Arc<AtomicBool>>, bytes: Bytes) -> Response {
    if fail.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(json!({
        "metadata":{"synthetic":true,"received_audio_sha256":sha256(&bytes)},
        "results":{"channels":[{"alternatives":[{
            "transcript":"synthetic fixture", "confidence":1.0,
            "words":[{"word":"synthetic","start":0.0,"end":0.5,"confidence":1.0}]
        }]}]}
    }))
    .into_response()
}

#[tokio::test]
async fn real_http_pipeline_transcribes_exact_canonical_bytes_and_recovers_provider_failure_without_duplicate_success()
 {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        SharedRecordingService::open(directory.path())
            .await
            .unwrap(),
    );
    let fail = Arc::new(AtomicBool::new(true));
    let provider = Server::start(
        Router::new()
            .route("/listen", post(fixture_provider))
            .with_state(fail.clone()),
    )
    .await;
    let transcriber = RecordingTranscriber::new(
        "deepgram",
        provider.base.clone(),
        String::new(),
        "fixture-v1".into(),
    )
    .unwrap();
    let processor = Arc::new(RecordingProcessor::new(
        service.clone(),
        transcriber,
        "synthetic-fixture".into(),
        "fixture-v1".into(),
    ));
    let token = "shared-local-test-credential-at-least-32-chars";
    let auth =
        Arc::new(StaticTokenAuthenticator::new([("workspace".into(), token.into())]).unwrap());
    let app = Server::start(
        shared_recordings::router(service.clone(), auth.clone())
            .merge(recording_transcription::router(processor, auth)),
    )
    .await;
    let client = reqwest::Client::new();
    let base = format!(
        "{}/v1/workspaces/workspace/shared-recordings/meeting",
        app.base
    );
    let response = client.post(&base).bearer_auth(token).json(&json!({
        "provider":"google_meet", "providerOccurrenceId":"conferenceRecords/fixture-call", "durationFrames":32000
    })).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let primary = encode_wav(&vec![100; 32000]).unwrap();
    let fallback = encode_wav(&vec![200; 32000]).unwrap();
    for (id, wav, rank, gaps) in [
        ("alice", &primary, 0, json!([{"start":16000,"end":24000}])),
        ("bob", &fallback, 1, json!([])),
    ] {
        let response = client.post(format!("{base}/captures/{id}")).bearer_auth(token).json(&json!({
            "metadata": {"uploaderId":id,"deviceId":id,"localCaptureId":id,"segmentSequence":0,
                "localSessionId":format!("session-{id}"),"localTranscriptId":format!("transcript-{id}"),
                "sha256":sha256(wav),"offsetFrames":0,"rank":rank,"gaps":gaps,"timingBasis":"operator_verified"},
            "wavBase64":STANDARD.encode(wav)
        })).send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().await.unwrap()
        );
    }
    let response = client
        .post(format!("{base}/process"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let pending = service
        .manifest("workspace", "meeting")
        .await
        .unwrap()
        .current
        .unwrap();
    assert_eq!(pending.transcript_status, "pending");
    let canonical = client
        .get(format!("{base}/audio"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(sha256(&canonical), pending.audio_sha256);
    fail.store(false, Ordering::SeqCst);
    let response = client
        .post(format!("{base}/process"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    let ready: Revision = client
        .get(format!("{base}/transcript"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ready.revision, pending.revision);
    let result = &ready.transcript.as_ref().unwrap().result;
    assert_eq!(
        result["metadata"]["received_audio_sha256"],
        ready.audio_sha256
    );
    assert_eq!(result["metadata"]["synthetic"], true);
    fail.store(true, Ordering::SeqCst);
    let retried: Revision = client
        .post(format!("{base}/process"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retried.transcript, ready.transcript);
    assert_eq!(retried.number, ready.number);
    let changed_model = RecordingProcessor::new(
        service.clone(),
        RecordingTranscriber::new(
            "deepgram",
            provider.base.clone(),
            String::new(),
            "fixture-v2".into(),
        )
        .unwrap(),
        "synthetic-fixture".into(),
        "fixture-v2".into(),
    );
    assert!(matches!(
        changed_model
            .process("workspace", "meeting")
            .await
            .unwrap_err()
            .downcast_ref::<shared_recordings::RecordingError>(),
        Some(shared_recordings::RecordingError::Conflict)
    ));
    let wrong_workspace = client
        .post(base.replace("/workspace/", "/other/") + "/process")
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_workspace.status(), StatusCode::FORBIDDEN);
    let _manifest: Value = client
        .get(base)
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
}
