use std::sync::Arc;

use anarlog_enterprise_control_plane::{
    auth::StaticTokenAuthenticator,
    shared_recordings::{
        CaptureMetadata, CreateOccurrence, Provider, RecordingError, SharedRecordingService,
        TimingBasis, Transcript, router,
    },
};
use anlg_shared_recording::{FrameRange, SAMPLE_RATE, decode_source, encode_wav, sha256};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tower::ServiceExt;

fn occurrence(id: &str) -> CreateOccurrence {
    CreateOccurrence {
        provider: Provider::GoogleMeet,
        provider_occurrence_id: format!("conferenceRecords/{id}"),
        duration_frames: 12,
    }
}
fn metadata(id: &str, wav: &[u8], rank: u32, gaps: Vec<FrameRange>) -> CaptureMetadata {
    CaptureMetadata {
        uploader_id: format!("user-{id}"),
        device_id: format!("device-{id}"),
        local_capture_id: id.into(),
        segment_sequence: 0,
        local_session_id: format!("session-{id}"),
        local_transcript_id: format!("transcript-{id}"),
        sha256: sha256(wav),
        offset_frames: 0,
        rank,
        gaps,
        timing_basis: TimingBasis::OperatorVerified,
    }
}

#[tokio::test]
async fn immutable_inputs_recover_without_duplicates_and_late_revisions_invalidate_stale_transcripts()
 {
    let directory = tempfile::tempdir().unwrap();
    let service = SharedRecordingService::open(directory.path())
        .await
        .unwrap();
    assert!(
        SharedRecordingService::open(directory.path())
            .await
            .is_err()
    );
    let first = encode_wav(&[10; 12]).unwrap();
    let fallback = encode_wav(&[20; 12]).unwrap();
    service
        .create_occurrence("workspace", "meeting", occurrence("actual-call"))
        .await
        .unwrap();
    let first_metadata = metadata("first", &first, 0, vec![FrameRange { start: 4, end: 8 }]);
    service
        .upload(
            "workspace",
            "meeting",
            "first",
            first_metadata.clone(),
            &first,
        )
        .await
        .unwrap();
    service
        .upload(
            "workspace",
            "meeting",
            "first",
            first_metadata.clone(),
            &first,
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .manifest("workspace", "meeting")
            .await
            .unwrap()
            .captures
            .len(),
        1
    );
    let old = service.finalize("workspace", "meeting").await.unwrap();
    let simulated = Transcript {
        provider: "synthetic-fixture".into(),
        model: "fixture-v1".into(),
        result: json!({"text":"explicit simulated transcript"}),
    };
    service
        .save_transcript(
            "workspace",
            "meeting",
            &old.revision,
            &old.audio_sha256,
            simulated.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .finalize("workspace", "meeting")
            .await
            .unwrap()
            .number,
        old.number
    );
    let mut changed = first_metadata.clone();
    changed.rank = 1;
    assert!(matches!(
        service
            .upload("workspace", "meeting", "first", changed, &first)
            .await,
        Err(RecordingError::Conflict)
    ));
    service
        .upload(
            "workspace",
            "meeting",
            "second",
            metadata("second", &fallback, 1, vec![]),
            &fallback,
        )
        .await
        .unwrap();
    assert!(matches!(
        service
            .upload(
                "workspace",
                "meeting",
                "first-retry-with-new-route",
                first_metadata.clone(),
                &first
            )
            .await,
        Err(RecordingError::Conflict)
    ));
    let mut next_segment = first_metadata.clone();
    next_segment.segment_sequence = 1;
    next_segment.rank = 5;
    next_segment.gaps.clear();
    service
        .upload(
            "workspace",
            "meeting",
            "first-segment-one",
            next_segment,
            &first,
        )
        .await
        .unwrap();
    assert!(
        service
            .manifest("workspace", "meeting")
            .await
            .unwrap()
            .inputs_changed
    );
    let revised = service.finalize("workspace", "meeting").await.unwrap();
    assert_eq!(revised.number, 2);
    assert!(revised.transcript.is_none());
    let (_, expected_audio) = service
        .canonical_audio("workspace", "meeting")
        .await
        .unwrap();
    for entry in std::fs::read_dir(directory.path()).unwrap() {
        let path = entry.unwrap().path();
        if std::fs::read(&path).ok().as_deref() == Some(expected_audio.as_slice()) {
            std::fs::remove_file(path).unwrap();
        }
    }
    assert!(
        service
            .canonical_audio("workspace", "meeting")
            .await
            .is_err()
    );
    assert_eq!(
        service
            .finalize("workspace", "meeting")
            .await
            .unwrap()
            .number,
        revised.number
    );
    assert_eq!(
        service
            .canonical_audio("workspace", "meeting")
            .await
            .unwrap()
            .1,
        expected_audio
    );
    assert_eq!(
        service.seal("workspace", "meeting").await.unwrap().revision,
        revised.revision
    );
    assert_eq!(
        service.seal("workspace", "meeting").await.unwrap().number,
        2
    );
    assert!(
        service
            .manifest("workspace", "meeting")
            .await
            .unwrap()
            .sealed
    );
    assert!(matches!(
        service
            .upload(
                "workspace",
                "meeting",
                "late",
                metadata("late", &fallback, 0, vec![]),
                &fallback
            )
            .await,
        Err(RecordingError::Conflict)
    ));
    service
        .upload(
            "workspace",
            "meeting",
            "first",
            first_metadata.clone(),
            &first,
        )
        .await
        .unwrap();
    assert!(
        service
            .historical_revision("workspace", "meeting", 1)
            .await
            .unwrap()
            .transcript
            .is_some()
    );
    assert!(matches!(
        service
            .save_transcript(
                "workspace",
                "meeting",
                &old.revision,
                &old.audio_sha256,
                simulated
            )
            .await,
        Err(RecordingError::Conflict)
    ));
    let (_, wav) = service
        .canonical_audio("workspace", "meeting")
        .await
        .unwrap();
    let decoded = decode_source("result".into(), 0, 0, &wav, vec![]).unwrap();
    assert_eq!(
        decoded.samples,
        vec![10, 10, 10, 10, 20, 20, 20, 20, 10, 10, 10, 10]
    );
    service
        .create_occurrence("workspace", "tomorrow", occurrence("next-call"))
        .await
        .unwrap();
    assert!(matches!(
        service
            .upload("workspace", "tomorrow", "first", first_metadata, &first)
            .await,
        Err(RecordingError::Conflict)
    ));
    drop(service);
    let recovered = SharedRecordingService::open(directory.path())
        .await
        .unwrap();
    assert!(
        recovered
            .manifest("workspace", "meeting")
            .await
            .unwrap()
            .sealed
    );
    assert_eq!(
        recovered
            .manifest("workspace", "meeting")
            .await
            .unwrap()
            .current
            .unwrap()
            .revision,
        revised.revision
    );
    recovered
        .delete_capture("workspace", "meeting", "second")
        .await
        .unwrap();
    assert!(matches!(
        recovered
            .historical_revision("workspace", "meeting", 1)
            .await,
        Err(RecordingError::NotFound)
    ));
    assert!(matches!(
        recovered.canonical_audio("workspace", "meeting").await,
        Err(RecordingError::NotFound)
    ));
    let after_delete = recovered.finalize("workspace", "meeting").await.unwrap();
    assert_eq!(after_delete.number, 3);
    recovered
        .delete_capture("workspace", "meeting", "second")
        .await
        .unwrap();
    assert_eq!(
        recovered
            .manifest("workspace", "meeting")
            .await
            .unwrap()
            .current
            .unwrap()
            .number,
        3
    );
    recovered
        .delete_occurrence("workspace", "meeting")
        .await
        .unwrap();
    assert!(matches!(
        recovered
            .create_occurrence("workspace", "meeting", occurrence("actual-call"))
            .await,
        Err(RecordingError::Conflict)
    ));
    assert!(matches!(
        recovered
            .upload(
                "workspace",
                "meeting",
                "second",
                metadata("second", &fallback, 1, vec![]),
                &fallback
            )
            .await,
        Err(RecordingError::NotFound)
    ));
}

#[tokio::test]
async fn audio_alignment_extending_a_capture_preserves_its_corrected_tail() {
    let directory = tempfile::tempdir().unwrap();
    let service = SharedRecordingService::open(directory.path())
        .await
        .unwrap();
    service
        .automatic_admit(
            "workspace",
            "aligned-tail",
            Provider::GoogleMeet,
            "conferenceRecords/aligned-tail",
            Some(0),
        )
        .await
        .unwrap();
    let mut state = 42u32;
    let samples = (0..SAMPLE_RATE as usize * 32)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as i16) / 2
        })
        .collect::<Vec<_>>();
    let reference = encode_wav(&samples[..SAMPLE_RATE as usize * 30]).unwrap();
    let observed = encode_wav(&samples[SAMPLE_RATE as usize * 8..]).unwrap();
    let mut reference_metadata = metadata("reference", &reference, 0, vec![]);
    reference_metadata.timing_basis = TimingBasis::DeviceClockHint;
    let mut observed_metadata = metadata("observed", &observed, 1, vec![]);
    observed_metadata.timing_basis = TimingBasis::DeviceClockHint;
    observed_metadata.offset_frames = SAMPLE_RATE as i64 * 8 - 1000;
    service
        .upload(
            "workspace",
            "aligned-tail",
            "reference",
            reference_metadata,
            &reference,
        )
        .await
        .unwrap();
    service
        .upload(
            "workspace",
            "aligned-tail",
            "observed",
            observed_metadata,
            &observed,
        )
        .await
        .unwrap();
    let revision = service.finalize("workspace", "aligned-tail").await.unwrap();
    assert_eq!(revision.manifest.duration_frames, samples.len() as u64);
    let tail = revision.manifest.selections.last().unwrap();
    assert_eq!(tail.capture_id.as_deref(), Some("observed"));
    assert_eq!(tail.end_frame, samples.len() as u64);
    let (_, wav) = service
        .canonical_audio("workspace", "aligned-tail")
        .await
        .unwrap();
    assert_eq!(
        decode_source("result".into(), 0, 0, &wav, vec![])
            .unwrap()
            .samples,
        samples
    );
}

#[tokio::test]
async fn http_requires_workspace_service_authority_and_rejects_room_aliases_and_incomplete_audio() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        SharedRecordingService::open(directory.path())
            .await
            .unwrap(),
    );
    let auth = Arc::new(
        StaticTokenAuthenticator::new([("workspace".into(), "operator-token".into())]).unwrap(),
    );
    let app = router(service.clone(), auth);
    let uri = "/v1/workspaces/workspace/shared-recordings/meeting";
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/workspaces/other/shared-recordings/meeting")
                .header(header::AUTHORIZATION, "Bearer operator-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut room = occurrence("call");
    room.provider_occurrence_id = "https://meet.google.com/abc-defg-hij".into();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::AUTHORIZATION, "Bearer operator-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&room).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::AUTHORIZATION, "Bearer operator-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&occurrence("call")).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{uri}/captures/first"))
                .header(header::AUTHORIZATION, "Bearer operator-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{\"wavBase64\":\"partial"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let manifest = service.manifest("workspace", "meeting").await.unwrap();
    assert!(manifest.captures.is_empty());
    let wav = encode_wav(&[42; 12]).unwrap();
    let request =
        json!({"metadata":metadata("first",&wav,0,vec![]),"wavBase64":STANDARD.encode(&wav)});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{uri}/captures/first"))
                .header(header::AUTHORIZATION, "Bearer operator-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        service
            .manifest("workspace", "meeting")
            .await
            .unwrap()
            .captures
            .len(),
        1
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, "Bearer operator-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(
        json["occurrence"]["providerOccurrenceId"],
        "conferenceRecords/call"
    );
}
