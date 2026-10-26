mod common;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use api_recordings::shared_recordings::FrameRange;
use api_recordings::{
    recording_transcription::{RecordingProcessor, RecordingTranscriber},
    shared_recordings::{CaptureMetadata, Provider, RecordingError, TimingBasis},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn each_original_is_transcribed_independently_and_partial_asr_retries_keep_cached_sources() {
    let (_storage, service) = common::service("workspace").await;
    let service = Arc::new(service);
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
    let expected_a = encode_wav(&[100; 32000]).unwrap();
    let expected_b = encode_wav(&[200; 32000]).unwrap();
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(move |request: &wiremock::Request| {
            let source = multipart_field(request, "file");
            if ![Some(expected_a.as_slice()),Some(expected_b.as_slice())].contains(&source)
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
                1 => {
                    if source == Some(expected_b.as_slice()) { ResponseTemplate::new(503) }
                    else { ResponseTemplate::new(200).set_body_json(json!({"text":"source A"})) }
                },
                5 => ResponseTemplate::new(200).set_body_json(json!({"text":if source == Some(expected_a.as_slice()) { "source A" } else { "source B" }})),
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
    assert_eq!(
        service
            .source_audio("workspace", "meeting", "alice")
            .await
            .unwrap()
            .1,
        primary
    );
    assert_eq!(
        service
            .source_audio("workspace", "meeting", "bob")
            .await
            .unwrap()
            .1,
        fallback
    );
    mode.store(1, Ordering::SeqCst);
    assert!(processor.process("workspace", "meeting").await.is_err());
    let partial = service.manifest("workspace", "meeting").await.unwrap();
    assert_eq!(partial.captures[0].transcript_status, "ready");
    assert_eq!(partial.captures[1].transcript_status, "pending");
    mode.store(5, Ordering::SeqCst);
    let ready = processor.process("workspace", "meeting").await.unwrap();
    assert_eq!(
        ready.captures[0].transcript.as_ref().unwrap().result,
        json!({"text":"source A"})
    );
    assert_eq!(
        ready.captures[1].transcript.as_ref().unwrap().result,
        json!({"text":"source B"})
    );
    mode.store(0, Ordering::SeqCst);
    let retried = processor.process("workspace", "meeting").await.unwrap();
    assert_eq!(retried.captures[0].transcript, ready.captures[0].transcript);
    assert_eq!(retried.captures[1].transcript, ready.captures[1].transcript);
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

fn encode_wav(samples: &[i16]) -> Result<Vec<u8>, hound::Error> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut bytes,
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
    Ok(bytes.into_inner())
}
fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
