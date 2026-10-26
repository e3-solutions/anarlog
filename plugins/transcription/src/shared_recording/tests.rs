use super::*;

fn job_fixture() -> Job {
    Job {
        id: "job".into(),
        session_id: "session".into(),
        state: "preparing".into(),
        error: None,
        user_id: "user".into(),
        organization_id: "org".into(),
        device_id: "device".into(),
        api_base_url: "http://localhost:8789".into(),
        transcript_id: "transcript".into(),
        started_at_ms: 0,
        stopped_at_ms: 1000,
        binding: None,
        segments: vec![Segment {
            sha256: "a".repeat(64),
            frames: 16100,
        }],
        gaps: vec![Gap {
            start: 8000,
            end: 9000,
        }],
        raw_frames: Some(16000),
    }
}

#[tokio::test]
async fn unverified_capture_quarantine_survives_a_late_admission_and_blocks_cached_uploads() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api_base_url = format!("http://{}", listener.local_addr().unwrap());
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier =
        format!("ai.e3group.shared-quarantine-test-{}", uuid::Uuid::new_v4());
    let app = tauri::test::mock_builder()
        .manage(QueueState::default())
        .build(context)
        .unwrap();
    let handle = app.handle().clone();
    let config = Configuration {
        enabled: true,
        user_id: "user".into(),
        organization_id: "org".into(),
        api_base_url: api_base_url.clone(),
        connections: vec![Connection {
            integration_id: "slack".into(),
            connection_id: "connection".into(),
            status: "connected".into(),
        }],
    };
    shared_recording_configure(handle.clone(), config.clone())
        .await
        .unwrap();
    let observe = |session: &str, platform: &str| {
        shared_recording_observe(
            handle.clone(),
            session.into(),
            platform.into(),
            None,
            chrono::Utc::now().timestamp_millis(),
            "token".into(),
            "user".into(),
            None,
            None,
        )
    };
    assert!(start_raw(&handle, "unconnected-provider", Some("user")));
    assert!(observe("unconnected-provider", "googleMeet").await.is_err());
    assert!(observe("unconnected-provider", "slack").await.is_err());
    assert!(start_raw(&handle, "before-identity", Some("user")));
    assert_eq!(
        observe("before-identity", "unverified").await.unwrap(),
        None
    );
    assert!(observe("before-identity", "slack").await.is_err());
    assert!(start_raw(&handle, "stopped", Some("user")));
    let stopped_at_ms = chrono::Utc::now().timestamp_millis();
    snapshot_stopped(&handle, "stopped", None, None, stopped_at_ms);
    assert!(
        shared_recording_observe(
            handle.clone(),
            "stopped".into(),
            "unverified".into(),
            None,
            stopped_at_ms + 1,
            "".into(),
            "user".into(),
            None,
            None,
        )
        .await
        .is_err()
    );
    let dir = directory(&handle).unwrap();
    assert!(
        read::<Binding>(&dir.join(format!("binding-{}.json", key("stopped"))))
            .unwrap()
            .is_none()
    );
    assert!(start_raw(&handle, "stopping", Some("user")));
    let observed_at_ms = chrono::Utc::now().timestamp_millis();
    snapshot_stopped(&handle, "stopping", None, None, observed_at_ms + 1);
    assert_eq!(
        shared_recording_observe(
            handle.clone(),
            "stopping".into(),
            "unverified".into(),
            None,
            observed_at_ms,
            "".into(),
            "user".into(),
            None,
            None,
        )
        .await
        .unwrap(),
        None
    );
    assert!(start_raw(&handle, "disabled", Some("user")));
    let state = handle.state::<QueueState>();
    let gate = state.serial.lock().await;
    let disable = shared_recording_configure(
        handle.clone(),
        Configuration {
            enabled: false,
            ..config.clone()
        },
    );
    let check_configuration_boundary = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if read::<Binding>(&dir.join(format!("binding-{}.json", key("disabled"))))
                    .unwrap()
                    .is_some_and(|binding| binding.admission.is_null())
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        drop(gate);
    };
    let (disabled, ()) = tokio::join!(disable, check_configuration_boundary);
    disabled.unwrap();
    shared_recording_configure(handle.clone(), config)
        .await
        .unwrap();
    assert!(observe("disabled", "slack").await.is_err());
    assert!(start_raw(&handle, "in-flight", Some("user")));
    assert!(start_raw(&handle, "first-failure", Some("user")));
    let (ready, received) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut ready = Some(ready);
        for status in [503, 200] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let length = String::from_utf8_lossy(&headers)
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            stream.read_exact(&mut vec![0; length]).unwrap();
            if status == 200 {
                ready.take().unwrap().send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
            let body = if status == 200 {
                r#"{"occurrenceId":"late"}"#
            } else {
                "{}"
            };
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    assert!(observe("first-failure", "slack").await.is_err());
    assert!(
        read::<Binding>(&dir.join(format!("binding-{}.json", key("first-failure"))))
            .unwrap()
            .unwrap()
            .admission
            .is_null()
    );
    assert!(observe("first-failure", "slack").await.is_err());
    let mut job = job_fixture();
    job.session_id = "in-flight".into();
    job.api_base_url = api_base_url;
    job.binding = Some(json!({"occurrenceId":"late"}));
    private_directory(&dir.join(&job.id)).unwrap();
    write_private(
        &dir.join(&job.id).join("original.audio"),
        b"original retained",
    )
    .unwrap();
    save(&job_path(&dir, &job.id), &job).unwrap();
    let quarantine = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), received)
            .await
            .unwrap()
            .unwrap();
        let gate = state.serial.lock().await;
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                observe("in-flight", "unverified"),
            )
            .await
            .unwrap()
            .unwrap(),
            None
        );
        assert!(
            read::<Binding>(&dir.join(format!("binding-{}.json", key("in-flight"))))
                .unwrap()
                .unwrap()
                .admission
                .is_null()
        );
        shared_recording_flush(handle.clone(), "token".into(), "user".into())
            .await
            .unwrap();
        assert_eq!(
            shared_recording_queue(handle.clone(), "user".into())
                .await
                .unwrap()[0]
                .state,
            "blocked_identity"
        );
        release.send(()).unwrap();
        drop(gate);
    };
    let (admission, ()) = tokio::join!(observe("in-flight", "slack"), quarantine);
    assert!(admission.is_err());
    assert!(observe("in-flight", "slack").await.is_err());
    let jobs = shared_recording_queue(handle.clone(), "user".into())
        .await
        .unwrap();
    assert_eq!(jobs[0].state, "blocked_identity");
    assert!(jobs[0].binding.is_none());
    assert!(
        jobs[0]
            .error
            .as_deref()
            .unwrap()
            .contains("start a new recording")
    );
    assert!(dir.join(&job.id).join("original.audio").exists());
    server.join().unwrap();
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn temporary_admission_failure_blocks_uploads_until_the_same_meeting_is_reverified() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for (status, body, expected) in [
            (200, r#"{"occurrenceId":"first"}"#, "/admit"),
            (503, "{}", "/admit"),
            (503, "{}", "/admit"),
            (200, r#"{"occurrenceId":"first"}"#, "/admit"),
            (200, "{}", "/captures/"),
            (200, "{}", "/process"),
            (200, "{}", "/captures/"),
            (200, "{}", "/process"),
            (200, r#"{"occurrenceId":"first"}"#, "/admit"),
            (200, r#"{"occurrenceId":"different"}"#, "/admit"),
        ] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "missing request");
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let length = stream.read(&mut buffer).unwrap();
                assert!(length > 0);
                request.extend_from_slice(&buffer[..length]);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + content_length {
                        break;
                    }
                }
            }
            assert!(
                String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap()
                    .contains(expected)
            );
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let root = std::env::temp_dir().join(format!("shared-revalidation-{}", uuid::Uuid::new_v4()));
    let session_id = uuid::Uuid::new_v4().to_string();
    let session_dir = root.join("sessions").join(&session_id);
    private_directory(&session_dir).unwrap();
    let audio = session_dir.join("audio.wav");
    let mut writer = hound::WavWriter::create(
        &audio,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for _ in 0..16000 {
        writer.write_sample(1000i16).unwrap();
    }
    writer.finalize().unwrap();
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = format!(
        "ai.e3group.shared-revalidation-test-{}",
        uuid::Uuid::new_v4()
    );
    let app = tauri::test::mock_builder()
        .manage(tauri_plugin_fs_sync::SessionAudioLocks::default())
        .manage(tauri_plugin_settings::StartupSnapshot::new(root.clone()))
        .manage(QueueState::default())
        .build(context)
        .unwrap();
    let handle = app.handle().clone();
    shared_recording_configure(
        handle.clone(),
        Configuration {
            enabled: true,
            user_id: "user".into(),
            organization_id: "org".into(),
            api_base_url: format!("http://{address}"),
            connections: vec![Connection {
                integration_id: "slack".into(),
                connection_id: "connection".into(),
                status: "connected".into(),
            }],
        },
    )
    .await
    .unwrap();
    assert!(start_raw(&handle, &session_id, Some("user")));
    observe_raw(&handle, &session_id, 16000, true);
    let observe = |call_identity| {
        shared_recording_observe(
            handle.clone(),
            session_id.clone(),
            "slack".into(),
            call_identity,
            0,
            "token".into(),
            "user".into(),
            None,
            None,
        )
    };
    assert_eq!(observe(None).await.unwrap().as_deref(), Some("first"));
    let before = shared_recording_enqueue(
        handle.clone(),
        session_id.clone(),
        "before".into(),
        audio.to_string_lossy().into_owned(),
        0,
        1000,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(before.state, "pending");
    assert!(observe(Some("slack://room/first".into())).await.is_err());
    let path = directory(&handle)
        .unwrap()
        .join(format!("binding-{}.json", key(&session_id)));
    let held: Binding = read(&path).unwrap().unwrap();
    assert!(held.verification_pending);
    assert_eq!(held.admission["occurrenceId"], "first");
    let after = shared_recording_enqueue(
        handle.clone(),
        session_id.clone(),
        "after".into(),
        audio.to_string_lossy().into_owned(),
        0,
        1000,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(after.state, "awaiting_identity");
    assert!(after.binding.is_none());
    shared_recording_flush(handle.clone(), "token".into(), "user".into())
        .await
        .unwrap();
    assert!(
        shared_recording_queue(handle.clone(), "user".into())
            .await
            .unwrap()
            .iter()
            .all(|job| job.state != "uploaded")
    );
    shared_recording_flush(handle.clone(), "token".into(), "user".into())
        .await
        .unwrap();
    assert!(
        !read::<Binding>(&path)
            .unwrap()
            .unwrap()
            .verification_pending
    );
    assert!(
        shared_recording_queue(handle.clone(), "user".into())
            .await
            .unwrap()
            .iter()
            .all(|job| job.state == "uploaded")
    );
    assert_eq!(
        observe(Some("slack://room/first".into()))
            .await
            .unwrap()
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        read::<Binding>(&path)
            .unwrap()
            .unwrap()
            .call_identity
            .as_deref(),
        Some("slack://room/first")
    );
    assert!(observe(Some("slack://room/first".into())).await.is_err());
    assert!(read::<Binding>(&path).unwrap().unwrap().admission.is_null());
    assert!(observe(Some("slack://room/first".into())).await.is_err());
    server.join().unwrap();
    std::fs::remove_dir_all(directory(&handle).unwrap().parent().unwrap()).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn enqueue_uses_the_authenticated_capture_owner_without_a_local_note_owner() {
    let root = std::env::temp_dir().join(format!("shared-owner-{}", uuid::Uuid::new_v4()));
    let session_id = uuid::Uuid::new_v4().to_string();
    let session_dir = root.join("sessions").join(&session_id);
    private_directory(&session_dir).unwrap();
    let audio = session_dir.join("audio.wav");
    let mut writer = hound::WavWriter::create(
        &audio,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for _ in 0..16000 {
        writer.write_sample(1000i16).unwrap();
    }
    writer.finalize().unwrap();
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier =
        format!("ai.e3group.shared-owner-test-{}", uuid::Uuid::new_v4());
    let app = tauri::test::mock_builder()
        .manage(tauri_plugin_fs_sync::SessionAudioLocks::default())
        .manage(tauri_plugin_settings::StartupSnapshot::new(root.clone()))
        .manage(QueueState::default())
        .build(context)
        .unwrap();
    let handle = app.handle().clone();
    let config = Configuration {
        enabled: true,
        user_id: "auth-user".into(),
        organization_id: "org".into(),
        api_base_url: "http://localhost:8789".into(),
        connections: Vec::new(),
    };
    shared_recording_configure(handle.clone(), config.clone())
        .await
        .unwrap();
    assert!(
        shared_recording_enqueue(
            handle.clone(),
            session_id.clone(),
            "transcript".into(),
            audio.to_string_lossy().into_owned(),
            0,
            1000
        )
        .await
        .is_err()
    );
    assert!(start_raw(&handle, &session_id, Some("auth-user")));
    observe_raw(&handle, &session_id, 16000, true);
    let job = shared_recording_enqueue(
        handle.clone(),
        session_id.clone(),
        "transcript".into(),
        audio.to_string_lossy().into_owned(),
        0,
        1000,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(job.user_id, "auth-user");
    assert_eq!(job.state, "awaiting_identity");
    shared_recording_configure(
        handle.clone(),
        Configuration {
            user_id: "another-account".into(),
            ..config
        },
    )
    .await
    .unwrap();
    assert!(
        shared_recording_enqueue(
            handle.clone(),
            session_id,
            "transcript".into(),
            audio.to_string_lossy().into_owned(),
            0,
            1000
        )
        .await
        .err()
        .unwrap()
        .contains("different sharing account")
    );
    std::fs::remove_dir_all(directory(&handle).unwrap().parent().unwrap()).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn codec_padding_keeps_failed_frames_uncovered_and_missing_frames_block_sharing() {
    let mut job = job_fixture();
    finish_preparation(&mut job);
    assert_eq!((job.gaps[0].start, job.gaps[0].end), (8000, 9100));
    job.raw_frames = Some(17000);
    finish_preparation(&mut job);
    assert_eq!(job.state, "blocked_timeline");
}

#[test]
fn uploaded_retry_audio_is_removed_without_deleting_the_receipt() {
    let dir = std::env::temp_dir().join(format!("shared-outbox-cleanup-{}", uuid::Uuid::new_v4()));
    private_directory(&dir.join("job")).unwrap();
    let mut job = job_fixture();
    job.state = "uploaded".into();
    save(&job_path(&dir, &job.id), &job).unwrap();
    write_private(&dir.join("job/original.audio"), b"original").unwrap();
    write_private(&dir.join("job/0.wav"), b"segment").unwrap();
    cleanup_uploaded_audio(&dir, &job).unwrap();
    cleanup_uploaded_audio(&dir, &job).unwrap();
    assert!(!dir.join("job/original.audio").exists());
    assert!(!dir.join("job/0.wav").exists());
    assert_eq!(
        read::<Job>(&job_path(&dir, &job.id))
            .unwrap()
            .unwrap()
            .state,
        "uploaded"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn upload_snapshot_is_decodable_mono_and_keeps_captured_silence() {
    let dir = std::env::temp_dir().join(format!(
        "shared-recording-normalize-{}",
        uuid::Uuid::new_v4()
    ));
    private_directory(&dir).unwrap();
    let input = dir.join("original.wav");
    let output = dir.join("outbox");
    private_directory(&output).unwrap();
    let mut writer = hound::WavWriter::create(
        &input,
        hound::WavSpec {
            channels: 2,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..48000 {
        let sample = if frame < 24000 { 8000i16 } else { 0 };
        writer.write_sample(sample).unwrap();
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
    let segments = normalize(&input, &output).unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].frames, 16000);
    let mut reader = hound::WavReader::open(output.join("0.wav")).unwrap();
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.spec().sample_rate, 16000);
    let samples = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(samples[1000] > 7000);
    assert_eq!(samples[15000], 0);
    assert!(input.exists());
    let hash = segments[0].sha256.clone();
    assert_eq!(normalize(&input, &output).unwrap()[0].sha256, hash);
    std::fs::remove_dir_all(dir).unwrap();
}
