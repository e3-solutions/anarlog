use api_recordings::shared_recordings::{
    CaptureMetadata, FrameRange, Provider, RecordingError, SharedRecordingService, TimingBasis,
    Transcript,
};
use serde_json::json;
use sha2::{Digest, Sha256};
mod common;
use wiremock::{
    Mock, ResponseTemplate,
    matchers::{method, path, path_regex},
};

fn wav(samples: &[i16]) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(
        &mut bytes,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for sample in samples {
        writer.write_sample(*sample).unwrap();
    }
    writer.finalize().unwrap();
    bytes.into_inner()
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn metadata(id: &str, bytes: &[u8]) -> CaptureMetadata {
    CaptureMetadata {
        uploader_id: format!("user-{id}"),
        device_id: id.into(),
        local_capture_id: id.into(),
        segment_sequence: 0,
        local_session_id: format!("session-{id}"),
        local_transcript_id: format!("text-{id}"),
        sha256: sha(bytes),
        offset_frames: -4,
        rank: 0,
        gaps: vec![FrameRange { start: 4, end: 8 }],
        timing_basis: TimingBasis::DeviceClockHint,
    }
}
async fn admit(service: &SharedRecordingService) {
    service
        .admit_actor(
            "org",
            "meeting",
            "user-a",
            Provider::GoogleMeet,
            "conferenceRecords/actual-call",
            Some(7),
        )
        .await
        .unwrap();
}
fn transcript(text: &str) -> Transcript {
    Transcript {
        provider: "openai".into(),
        model: "tiny.en".into(),
        result: json!({"text":text}),
    }
}

#[tokio::test]
async fn original_source_integrity_and_storage_failures_remain_visible() {
    let (upstream, service) = common::service("org").await;
    admit(&service).await;
    let original = wav(&[10; 12]);
    service
        .upload("org", "meeting", "a", metadata("a", &original), &original)
        .await
        .unwrap();
    let (source, bytes) = service.source_audio("org", "meeting", "a").await.unwrap();
    assert_eq!(bytes, original);
    assert_eq!(source.frame_count, 12);
    assert_eq!(source.metadata.offset_frames, -4);
    assert_eq!(source.metadata.gaps, vec![FrameRange { start: 4, end: 8 }]);
    assert_eq!(source.transcript_status, "pending");
    service
        .save_source_transcript(
            "org",
            "meeting",
            "a",
            &sha(&original),
            transcript("source A"),
        )
        .await
        .unwrap();
    let reconnected = api_recordings::shared_recordings::SharedRecordingService::supabase(
        &upstream.uri(),
        "fixture-service-role",
    )
    .unwrap();
    assert_eq!(
        reconnected
            .source_audio("org", "meeting", "a")
            .await
            .unwrap()
            .0
            .transcript,
        Some(transcript("source A"))
    );
    Mock::given(method("GET"))
        .and(path_regex(
            "^/storage/v1/object/authenticated/recording-audio/",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(wav(&[99; 12])))
        .with_priority(1)
        .mount(&upstream)
        .await;
    assert!(matches!(
        service.source_audio("org", "meeting", "a").await,
        Err(RecordingError::Conflict)
    ));
    Mock::given(method("GET"))
        .and(path("/rest/v1/recording_captures"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&upstream)
        .await;
    assert!(matches!(
        service.manifest("org", "meeting").await,
        Err(RecordingError::Upstream)
    ));
}

#[tokio::test]
async fn provider_aliases_incomplete_audio_and_rpc_errors_fail_closed() {
    let (upstream, service) = common::service("org").await;
    assert!(matches!(
        service
            .admit_actor(
                "org",
                "meeting",
                "user-a",
                Provider::GoogleMeet,
                "https://meet.google.com/abc-defg-hij",
                Some(0)
            )
            .await,
        Err(RecordingError::Invalid(_))
    ));
    admit(&service).await;
    let original = wav(&[42; 12]);
    assert!(matches!(
        service
            .upload(
                "org",
                "meeting",
                "a",
                metadata("a", &original),
                &original[..original.len() - 1]
            )
            .await,
        Err(RecordingError::Invalid(_))
    ));
    assert!(
        service
            .manifest("org", "meeting")
            .await
            .unwrap()
            .captures
            .is_empty()
    );
    service
        .upload("org", "meeting", "a", metadata("a", &original), &original)
        .await
        .unwrap();
    let mode = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
    let responder_mode = mode.clone();
    Mock::given(method("POST")).and(path("/rest/v1/rpc/recording_seal"))
        .respond_with(move |_: &wiremock::Request| ResponseTemplate::new(400).set_body_json(json!({"code":match responder_mode.load(std::sync::atomic::Ordering::SeqCst) {0=>"23505",1=>"P0002",2=>"22023",_=>"unexpected"}})))
        .with_priority(1).mount(&upstream).await;
    for value in 0..4 {
        mode.store(value, std::sync::atomic::Ordering::SeqCst);
        let error = service.seal("org", "meeting").await.unwrap_err();
        assert!(matches!(
            (value, error),
            (0, RecordingError::Conflict)
                | (1, RecordingError::NotFound)
                | (2, RecordingError::Invalid(_))
                | (3, RecordingError::Upstream)
        ));
    }
    assert_eq!(
        service.source_audio("org", "meeting", "a").await.unwrap().1,
        original
    );
}
