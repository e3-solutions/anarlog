use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use anarlog_enterprise_control_plane::{
    recording_transcription::{RecordingProcessor, RecordingTranscriber},
    shared_recordings::{
        CaptureMetadata, Provider, RecordingError, SharedRecordingService, TimingBasis,
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
    for (url, model) in [
        ("http://provider.invalid", "tiny.en"),
        ("https://user:secret@provider.invalid", "tiny.en"),
        ("https://provider.invalid", "invalid model"),
        ("https://provider.invalid", ""),
    ] {
        assert!(RecordingTranscriber::new(url.into(), String::new(), model.into()).is_err());
    }
    let mode = Arc::new(AtomicU8::new(0));
    let provider = MockServer::start().await;
    let redirect = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "redirected"})))
        .mount(&redirect)
        .await;
    let redirect_url = redirect.uri();
    let provider_mode = mode.clone();
    let expected =
        encode_wav(&[vec![100; 16000], vec![200; 8000], vec![100; 8000]].concat()).unwrap();
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(move |request: &wiremock::Request| {
            if multipart_field(request, "file") != Some(expected.as_slice())
                || multipart_field(request, "model") != Some(b"fixture-v1")
                || multipart_field(request, "response_format") != Some(b"json")
                || request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    != Some("Bearer provider-fixture-key")
            {
                return ResponseTemplate::new(400);
            }
            match provider_mode.load(Ordering::SeqCst) {
                0 => ResponseTemplate::new(503),
                1 => ResponseTemplate::new(200)
                    .set_body_json(json!({"text": "  synthetic fixture\n"})),
                2 => ResponseTemplate::new(200)
                    .set_body_json(json!({"text": "x".repeat(8 * 1024 * 1024)})),
                3 => ResponseTemplate::new(302).insert_header("Location", redirect_url.as_str()),
                _ => ResponseTemplate::new(200).set_body_json(json!({"text": 42})),
            }
        })
        .mount(&provider)
        .await;
    let processor = RecordingProcessor::new(
        service.clone(),
        RecordingTranscriber::new(
            provider.uri(),
            "provider-fixture-key".into(),
            "fixture-v1".into(),
        )
        .unwrap(),
    );
    service
        .admit_actor(
            "workspace",
            "meeting",
            "alice",
            Provider::GoogleMeet,
            "conferenceRecords/fixture-call",
            Some(0),
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
    for failure in [0, 2, 3, 4] {
        mode.store(failure, Ordering::SeqCst);
        assert!(processor.process("workspace", "meeting").await.is_err());
    }
    let (pending, canonical) = service
        .canonical_audio("workspace", "meeting")
        .await
        .unwrap();
    assert_eq!(pending.transcript_status, "pending");
    assert_eq!(sha256(&canonical), pending.audio_sha256);
    mode.store(1, Ordering::SeqCst);
    let ready = processor.process("workspace", "meeting").await.unwrap();
    assert_eq!(ready.revision, pending.revision);
    let result = &ready.transcript.as_ref().unwrap().result;
    assert_eq!(
        result,
        &json!({
            "metadata": {"timing_source": "synthetic_text"},
            "results": {"channels": [{"alternatives": [{
                "transcript": "synthetic fixture", "confidence": 1.0, "words": []
            }]}]}
        })
    );
    assert_eq!(ready.transcript.as_ref().unwrap().provider, "openai");
    assert_eq!(ready.transcript.as_ref().unwrap().model, "fixture-v1");
    mode.store(0, Ordering::SeqCst);
    let retried = processor.process("workspace", "meeting").await.unwrap();
    assert_eq!(retried.transcript, ready.transcript);
    assert_eq!(retried.number, ready.number);
    let changed_model = RecordingProcessor::new(
        service.clone(),
        RecordingTranscriber::new(provider.uri(), String::new(), "fixture-v2".into()).unwrap(),
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

fn multipart_field<'a>(request: &'a wiremock::Request, name: &str) -> Option<&'a [u8]> {
    let content_type = request.headers.get("content-type")?.to_str().ok()?;
    let boundary = content_type.split("boundary=").nth(1)?.trim_matches('"');
    let field = format!("name=\"{name}\"");
    let header = request
        .body
        .windows(field.len())
        .position(|bytes| bytes == field.as_bytes())?;
    let offset = request.body[header..]
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")?
        + header
        + 4;
    let delimiter = format!("\r\n--{boundary}");
    let end = request.body[offset..]
        .windows(delimiter.len())
        .position(|bytes| bytes == delimiter.as_bytes())?
        + offset;
    Some(&request.body[offset..end])
}
