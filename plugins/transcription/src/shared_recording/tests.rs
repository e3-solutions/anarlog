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

struct Fixture {
    app: tauri::App<tauri::test::MockRuntime>,
    root: std::path::PathBuf,
    session: String,
    audio: std::path::PathBuf,
    config: Configuration,
}

impl Fixture {
    async fn new(user: &str, api_base_url: String, connected: bool) -> Self {
        let root =
            std::env::temp_dir().join(format!("shared-recording-test-{}", uuid::Uuid::new_v4()));
        let session = uuid::Uuid::new_v4().to_string();
        let audio = root.join("sessions").join(&session).join("audio.wav");
        private_directory(audio.parent().unwrap()).unwrap();
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
            format!("ai.e3group.shared-test-{}", uuid::Uuid::new_v4());
        let app = tauri::test::mock_builder()
            .manage(tauri_plugin_fs_sync::SessionAudioLocks::default())
            .manage(tauri_plugin_settings::StartupSnapshot::new(root.clone()))
            .manage(QueueState::default())
            .manage(std::sync::Arc::new(crate::BatchSessionRegistry::default()))
            .build(context)
            .unwrap();
        let config = Configuration {
            enabled: true,
            user_id: user.into(),
            organization_id: "org".into(),
            api_base_url,
            connections: if connected {
                vec![Connection {
                    integration_id: "slack".into(),
                    connection_id: "connection".into(),
                    status: "connected".into(),
                }]
            } else {
                Vec::new()
            },
        };
        shared_recording_configure(app.handle().clone(), config.clone())
            .await
            .unwrap();
        Self {
            app,
            root,
            session,
            audio,
            config,
        }
    }

    async fn enqueue(&self, transcript: &str) -> Result<Option<Job>, String> {
        shared_recording_enqueue(
            self.app.handle().clone(),
            self.session.clone(),
            transcript.into(),
            self.audio.to_string_lossy().into_owned(),
            0,
            1000,
        )
        .await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(directory) = directory(self.app.handle()) {
            let _ = std::fs::remove_dir_all(directory.parent().unwrap());
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    use std::io::Read;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
    }
    let headers = String::from_utf8(headers).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    stream.read_exact(&mut vec![0; length]).unwrap();
    headers
}

#[tokio::test]
async fn unverified_capture_quarantine_survives_a_late_admission_and_blocks_cached_uploads() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api_base_url = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Fixture::new("user", api_base_url.clone(), true).await;
    let handle = fixture.app.handle().clone();
    let config = fixture.config.clone();
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
            read_request(&mut stream);
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
}

#[tokio::test]
async fn temporary_admission_failure_blocks_uploads_until_the_same_meeting_is_reverified() {
    use std::io::Write;
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
            assert!(
                read_request(&mut stream)
                    .lines()
                    .next()
                    .unwrap()
                    .contains(expected)
            );
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let fixture = Fixture::new("user", format!("http://{address}"), true).await;
    let handle = fixture.app.handle().clone();
    let session_id = fixture.session.clone();
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
    let before = fixture.enqueue("before").await.unwrap().unwrap();
    assert_eq!(before.state, "pending");
    assert!(observe(Some("slack://room/first".into())).await.is_err());
    let after = fixture.enqueue("after").await.unwrap().unwrap();
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
    assert!(observe(Some("slack://room/first".into())).await.is_err());
    assert!(observe(Some("slack://room/first".into())).await.is_err());
    server.join().unwrap();
}

#[tokio::test]
async fn enqueue_uses_the_authenticated_capture_owner_without_a_local_note_owner() {
    let fixture = Fixture::new("auth-user", "http://localhost:8789".into(), false).await;
    let handle = fixture.app.handle().clone();
    let session_id = fixture.session.clone();
    let config = fixture.config.clone();
    assert!(fixture.enqueue("transcript").await.is_err());
    assert!(start_raw(&handle, &session_id, Some("auth-user")));
    observe_raw(&handle, &session_id, 16000, true);
    let job = fixture.enqueue("transcript").await.unwrap().unwrap();
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
    assert!(fixture.enqueue("transcript").await.is_err());
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
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_playback_cleanup_only_removes_known_cache_files_without_following_symlinks() {
    let root =
        std::env::temp_dir().join(format!("shared-legacy-playback-{}", uuid::Uuid::new_v4()));
    let base = root.join("app");
    let playback = base.join("shared-recording-outbox/playback");
    private_directory(&playback).unwrap();
    let cached = playback.join(format!("{}.wav", "a".repeat(64)));
    let unrelated = playback.join("unrelated.wav");
    let source = base.join("shared-recording-outbox/source.audio");
    for path in [&cached, &unrelated, &source] {
        write_private(path, b"preserved audio").unwrap();
    }
    #[cfg(unix)]
    let linked_file = {
        let path = playback.join(format!("{}.wav", "b".repeat(64)));
        std::os::unix::fs::symlink(&source, &path).unwrap();
        path
    };
    clear_legacy_playback_at(&base);
    assert!(!cached.exists());
    assert!(unrelated.exists());
    assert!(source.exists());
    #[cfg(unix)]
    {
        assert!(
            linked_file
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        write_private(&cached, b"preserved target").unwrap();
        let linked_base = root.join("linked-app");
        std::os::unix::fs::symlink(&base, &linked_base).unwrap();
        clear_legacy_playback_at(&linked_base);
        assert!(cached.exists());
        for alias in [
            "shared-recording-outbox",
            "shared-recording-outbox/playback",
        ] {
            let alias_base = root.join(uuid::Uuid::new_v4().to_string());
            let link = alias_base.join(alias);
            private_directory(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(base.join(alias), &link).unwrap();
            clear_legacy_playback_at(&alias_base);
            assert!(cached.exists());
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn opting_out_cancels_inflight_upload_and_stays_excluded_after_restart() {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let fixture = Fixture::new(
        "user",
        format!("http://{}", listener.local_addr().unwrap()),
        true,
    )
    .await;
    let handle = fixture.app.handle().clone();
    let dir = directory(&handle).unwrap();
    let state = handle.state::<QueueState>();
    assert!(start_raw(&handle, &fixture.session, Some("user")));
    let binding = Binding {
        session_id: fixture.session.clone(),
        user_id: "user".into(),
        organization_id: "org".into(),
        api_base_url: fixture.config.api_base_url.clone(),
        admission: json!({"occurrenceId":"occurrence"}),
        observed_at_ms: 0,
        platform: "slack".into(),
        call_identity: None,
        verification_pending: false,
        connection_id: Some("connection".into()),
        quarantine_reason: None,
    };
    save(
        &dir.join(format!("binding-{}.json", key(&fixture.session))),
        &binding,
    )
    .unwrap();
    let job = fixture.enqueue("transcript").await.unwrap().unwrap();
    let (received, arrived) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert!(read_request(&mut stream).contains("/captures/"));
        received.send(()).unwrap();
        released
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    });
    let exclude = async {
        arrived.await.unwrap();
        assert!(exclude_session(&dir, &state, &fixture.session, "other-account").is_err());
        assert!(capture_allowed(&handle, &fixture.session));
        exclude_session(&dir, &state, &fixture.session, "user").unwrap();
        release.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(upload(&dir, &job, "token", &state), exclude);
    assert!(result.unwrap_err().contains("excluded"));
    assert!(!capture_allowed(&handle, &fixture.session));
    assert!(
        read::<Binding>(&dir.join(format!("binding-{}.json", key(&fixture.session))))
            .unwrap()
            .unwrap()
            .admission
            .is_null()
    );
    assert!(fixture.enqueue("later-transcript").await.is_err());
    // A fresh queue state still checks the persisted tombstone before any request.
    assert!(verify_upload_binding(&dir, &job, &QueueState::default()).is_err());
    state.excluded.lock().unwrap().clear();
    assert!(ensure_allowed(&handle, &fixture.session).is_err());
    assert!(!capture_allowed(&handle, &fixture.session));
    assert!(ensure_allowed(&handle, "intentional-new-session").is_ok());
    assert!(dir.join(&job.id).join("original.audio").exists());
    server.join().unwrap();
}

#[tokio::test]
async fn opt_out_stops_local_capture_without_shared_recording_configuration() {
    let fixture = Fixture::new("user", "http://localhost:8789".into(), false).await;
    let handle = fixture.app.handle().clone();
    let dir = directory(&handle).unwrap();
    std::fs::remove_file(dir.join("configuration.json")).unwrap();
    assert!(!start_raw(&handle, &fixture.session, Some("user")));
    let state = handle.state::<QueueState>();
    assert!(exclude_session(&dir, &state, &fixture.session, "other").is_err());
    for _ in 0..2 {
        assert_eq!(
            shared_recording_observe(
                handle.clone(),
                fixture.session.clone(),
                "unverified".into(),
                None,
                0,
                String::new(),
                "user".into(),
                Some("user_excluded".into()),
                None,
            )
            .await
            .unwrap(),
            None
        );
    }
    assert!(!capture_allowed(&handle, &fixture.session));
    assert!(ensure_allowed(&handle, &fixture.session).is_err());
}
