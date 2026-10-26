use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anarlog_enterprise_control_plane::{
    recording_transcription::{RecordingProcessor, RecordingTranscriber},
    shared_recordings::{
        CaptureMetadata, CreateOccurrence, Provider, RecordingError, SharedRecordingService,
        TimingBasis,
    },
};
use anlg_shared_recording::{FrameRange, encode_wav, sha256};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn real_http_provider_receives_exact_canonical_bytes_and_recovers_without_duplicate_success()
{
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        SharedRecordingService::open(directory.path())
            .await
            .unwrap(),
    );
    let fail = Arc::new(AtomicBool::new(true));
    let provider = MockServer::start().await;
    let provider_fail = fail.clone();
    Mock::given(method("POST"))
        .and(path("/listen"))
        .respond_with(move |request: &wiremock::Request| {
            if provider_fail.load(Ordering::SeqCst) {
                return ResponseTemplate::new(503);
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "metadata":{"synthetic":true,"received_audio_sha256":sha256(&request.body)},
                "results":{"channels":[{"alternatives":[{
                    "transcript":"synthetic fixture", "confidence":1.0,
                    "words":[{"word":"synthetic","start":0.0,"end":0.5,"confidence":1.0}]
                }]}]}
            }))
        })
        .mount(&provider)
        .await;
    let processor = RecordingProcessor::new(
        service.clone(),
        RecordingTranscriber::new(
            "deepgram",
            provider.uri(),
            String::new(),
            "fixture-v1".into(),
        )
        .unwrap(),
        "synthetic-fixture".into(),
        "fixture-v1".into(),
    );
    service
        .create_occurrence(
            "workspace",
            "meeting",
            CreateOccurrence {
                provider: Provider::GoogleMeet,
                provider_occurrence_id: "conferenceRecords/fixture-call".into(),
                duration_frames: 32000,
            },
        )
        .await
        .unwrap();
    let primary = encode_wav(&vec![100; 32000]).unwrap();
    let fallback = encode_wav(&vec![200; 32000]).unwrap();
    for (id, wav, rank, gaps) in [
        (
            "alice",
            &primary,
            0,
            vec![FrameRange {
                start: 16000,
                end: 24000,
            }],
        ),
        ("bob", &fallback, 1, vec![]),
    ] {
        service
            .upload(
                "workspace",
                "meeting",
                id,
                CaptureMetadata {
                    uploader_id: id.into(),
                    device_id: id.into(),
                    local_capture_id: id.into(),
                    segment_sequence: 0,
                    local_session_id: format!("session-{id}"),
                    local_transcript_id: format!("transcript-{id}"),
                    sha256: sha256(wav),
                    offset_frames: 0,
                    rank,
                    gaps,
                    timing_basis: TimingBasis::OperatorVerified,
                },
                wav,
            )
            .await
            .unwrap();
    }
    assert!(processor.process("workspace", "meeting").await.is_err());
    let (pending, canonical) = service
        .canonical_audio("workspace", "meeting")
        .await
        .unwrap();
    assert_eq!(pending.transcript_status, "pending");
    assert_eq!(sha256(&canonical), pending.audio_sha256);
    fail.store(false, Ordering::SeqCst);
    let ready = processor.process("workspace", "meeting").await.unwrap();
    assert_eq!(ready.revision, pending.revision);
    let result = &ready.transcript.as_ref().unwrap().result;
    assert_eq!(
        result["metadata"]["received_audio_sha256"],
        ready.audio_sha256
    );
    assert_eq!(result["metadata"]["synthetic"], true);
    fail.store(true, Ordering::SeqCst);
    let retried = processor.process("workspace", "meeting").await.unwrap();
    assert_eq!(retried.transcript, ready.transcript);
    assert_eq!(retried.number, ready.number);
    let changed_model = RecordingProcessor::new(
        service.clone(),
        RecordingTranscriber::new(
            "deepgram",
            provider.uri(),
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
            .downcast_ref::<RecordingError>(),
        Some(RecordingError::Conflict)
    ));
}
