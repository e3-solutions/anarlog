use api_recordings::shared_recordings::{
    CaptureMetadata, FrameRange, Provider, RecordingError, SharedRecordingService, TimingBasis,
    Transcript,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Connection;

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
async fn original_audio_and_source_transcripts_survive_retry_restart_and_revocation() {
    let directory = tempfile::tempdir().unwrap();
    let service = SharedRecordingService::open(directory.path())
        .await
        .unwrap();
    assert!(
        SharedRecordingService::open(directory.path())
            .await
            .is_err()
    );
    admit(&service).await;
    assert_eq!(
        service
            .admit_actor(
                "org",
                "meeting",
                "user-b",
                Provider::GoogleMeet,
                "conferenceRecords/actual-call",
                Some(99)
            )
            .await
            .unwrap(),
        Some(7)
    );
    assert!(
        service
            .actor_admitted("org", "meeting", "user-b")
            .await
            .unwrap()
    );
    assert!(matches!(
        service
            .admit_actor(
                "org",
                "meeting",
                "wrong-user",
                Provider::GoogleMeet,
                "conferenceRecords/other-call",
                Some(7)
            )
            .await,
        Err(RecordingError::Conflict)
    ));
    assert!(
        !service
            .actor_admitted("org", "meeting", "wrong-user")
            .await
            .unwrap()
    );
    let first = wav(&[10; 12]);
    let second = wav(&[20; 12]);
    for _ in 0..2 {
        service
            .upload("org", "meeting", "a", metadata("a", &first), &first)
            .await
            .unwrap();
    }
    service
        .upload("org", "meeting", "b", metadata("b", &second), &second)
        .await
        .unwrap();
    let mut changed = metadata("a", &first);
    changed.rank = 1;
    assert!(matches!(
        service.upload("org", "meeting", "a", changed, &first).await,
        Err(RecordingError::Conflict)
    ));
    assert!(matches!(
        service
            .save_source_transcript("org", "meeting", "a", "wrong-sha", transcript("source A"))
            .await,
        Err(RecordingError::Conflict)
    ));
    for _ in 0..2 {
        service
            .save_source_transcript("org", "meeting", "a", &sha(&first), transcript("source A"))
            .await
            .unwrap();
    }
    assert!(matches!(
        service
            .save_source_transcript(
                "org",
                "meeting",
                "a",
                &sha(&first),
                transcript("replacement")
            )
            .await,
        Err(RecordingError::Conflict)
    ));
    let manifest = service.manifest("org", "meeting").await.unwrap();
    assert_eq!(manifest.captures.len(), 2);
    assert!(manifest.current.is_none());
    assert_eq!(manifest.captures[0].transcript_status, "ready");
    assert_eq!(manifest.captures[1].transcript_status, "pending");
    let (source, original) = service.source_audio("org", "meeting", "a").await.unwrap();
    assert_eq!(original, first);
    assert_eq!(source.frame_count, 12);
    assert_eq!(source.metadata.offset_frames, -4);
    assert_eq!(source.transcript.unwrap(), transcript("source A"));
    assert_eq!(
        service.source_audio("org", "meeting", "b").await.unwrap().1,
        second
    );
    drop(service);
    let recovered = SharedRecordingService::open(directory.path())
        .await
        .unwrap();
    assert_eq!(
        recovered
            .source_audio("org", "meeting", "a")
            .await
            .unwrap()
            .0
            .transcript
            .unwrap(),
        transcript("source A")
    );
    recovered
        .delete_capture("org", "meeting", "a")
        .await
        .unwrap();
    assert!(matches!(
        recovered.source_audio("org", "meeting", "a").await,
        Err(RecordingError::NotFound)
    ));
    assert!(matches!(
        recovered
            .save_source_transcript("org", "meeting", "a", &sha(&first), transcript("late ASR"))
            .await,
        Err(RecordingError::NotFound)
    ));
    assert!(matches!(
        recovered
            .upload("org", "meeting", "a", metadata("a", &first), &first)
            .await,
        Err(RecordingError::Conflict)
    ));
    assert!(recovered.seal("org", "meeting").await.unwrap().sealed);
    recovered
        .upload("org", "meeting", "b", metadata("b", &second), &second)
        .await
        .unwrap();
    assert!(matches!(
        recovered
            .upload("org", "meeting", "c", metadata("c", &first), &first)
            .await,
        Err(RecordingError::Conflict)
    ));
    recovered.delete_occurrence("org", "meeting").await.unwrap();
    assert!(matches!(
        recovered.source_audio("org", "meeting", "b").await,
        Err(RecordingError::NotFound)
    ));
    assert!(
        !recovered
            .actor_admitted("org", "meeting", "user-a")
            .await
            .unwrap()
    );
    assert!(matches!(
        recovered
            .admit_actor(
                "org",
                "meeting",
                "user-a",
                Provider::GoogleMeet,
                "conferenceRecords/actual-call",
                Some(7)
            )
            .await,
        Err(RecordingError::Conflict)
    ));
}

#[tokio::test]
async fn upgrading_legacy_storage_preserves_original_audio_and_marks_old_shared_transcripts_legacy()
{
    let directory = tempfile::tempdir().unwrap();
    let service = SharedRecordingService::open(directory.path())
        .await
        .unwrap();
    admit(&service).await;
    let original = wav(&[42; 12]);
    service
        .upload("org", "meeting", "a", metadata("a", &original), &original)
        .await
        .unwrap();
    drop(service);
    // Recreate the previously shipped shape: no source ASR column and an old audio-derived revision.
    let mut database = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("recordings.sqlite")),
    )
    .await
    .unwrap();
    sqlx::query("ALTER TABLE captures DROP COLUMN transcript_json")
        .execute(&mut database)
        .await
        .unwrap();
    let legacy = json!({"number":1,"revision":"old-revision","audioSha256":sha(&original),"manifest":{"version":1,"sample_rate":16000,"duration_frames":12,"selections":[]},"transcript":{"provider":"openai","model":"tiny.en","result":{"results":{"channels":[{"alternatives":[{"transcript":"old combined ASR"}]}]}}},"transcriptStatus":"ready"});
    std::fs::write(directory.path().join("canonical-legacy.wav"), &original).unwrap();
    sqlx::query("INSERT INTO revisions(workspace,occurrence,number,revision,object,revision_json) VALUES('org','meeting',1,'old-revision','canonical-legacy.wav',?)").bind(legacy.to_string()).execute(&mut database).await.unwrap();
    sqlx::query("UPDATE occurrences SET current_json=?,next_revision=2 WHERE workspace='org' AND id='meeting'").bind(legacy.to_string()).execute(&mut database).await.unwrap();
    database.close().await.unwrap();
    let recovered = SharedRecordingService::open(directory.path())
        .await
        .unwrap();
    let current = recovered
        .manifest("org", "meeting")
        .await
        .unwrap()
        .current
        .unwrap();
    assert_eq!(current.kind, "legacy_audio");
    assert_eq!(current.revision, "old-revision");
    assert_eq!(current.audio_sha256, Some(sha(&original)));
    assert_eq!(
        recovered
            .source_audio("org", "meeting", "a")
            .await
            .unwrap()
            .1,
        original
    );
    recovered
        .save_source_transcript(
            "org",
            "meeting",
            "a",
            &sha(&original),
            transcript("independent source ASR"),
        )
        .await
        .unwrap();
    assert_eq!(
        recovered
            .manifest("org", "meeting")
            .await
            .unwrap()
            .current
            .unwrap()
            .kind,
        "legacy_audio"
    );
    assert!(directory.path().join("canonical-legacy.wav").is_file());
}

#[tokio::test]
async fn provider_aliases_and_incomplete_audio_cannot_admit_sources() {
    let directory = tempfile::tempdir().unwrap();
    let service = SharedRecordingService::open(directory.path())
        .await
        .unwrap();
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
}
