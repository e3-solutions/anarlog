use super::*;

fn current_slack_huddle_window() -> Vec<AxNode> {
    let bounds = Some(AxRect {
        x: 10.0,
        y: 10.0,
        width: 30.0,
        height: 30.0,
    });
    vec![
        node(
            0,
            "AXWindow",
            "capture-pilot-test-priyal - E3 group - Slack",
            None,
        ),
        node(1, "AXToolbar", "Huddles actions", None),
        node(2, "AXButton", "Unmute microphone", bounds.clone()),
        node(3, "AXButton", "Leave Huddle", bounds),
    ]
}

#[test]
fn slack_recording_accepts_live_controls_without_relaxing_chat_addressing() {
    let mut nodes = current_slack_huddle_window();
    for microphone in ["Unmute microphone", "Mute microphone"] {
        nodes[2].title = Some(microphone.to_string());
        assert_eq!(
            slack_huddle_recording_label(&nodes),
            Some("Slack huddle".to_string())
        );
        let bundle_id = "com.tinyspeck.slackmacgap";
        let platform = classify_platform(
            bundle_id,
            Some("Slack huddle"),
            &nodes,
            classify_bundle(bundle_id),
        );
        assert_eq!(platform, MeetingPlatform::Slack);
        assert_eq!(
            classify_surface(bundle_id, &platform),
            MeetingSurface::Native
        );
        assert!(scoped_meeting_is_active(&platform, &nodes, true));
        assert!(!native_meeting_window_is_validated(&platform, &nodes));
        assert_eq!(slack_huddle_context(&nodes), None);
    }
}

#[test]
fn slack_recording_rejects_prejoin_disabled_hidden_and_cross_window_controls() {
    let nodes = current_slack_huddle_window();
    for missing in [1, 2, 3] {
        let mut incomplete = nodes.clone();
        incomplete.remove(missing);
        assert_eq!(slack_huddle_recording_label(&incomplete), None);
        assert!(!scoped_meeting_is_active(
            &MeetingPlatform::Slack,
            &incomplete,
            true
        ));
    }
    for index in [2, 3] {
        let mut disabled = nodes.clone();
        disabled[index].enabled = Some(false);
        assert_eq!(slack_huddle_recording_label(&disabled), None);
        assert!(!scoped_meeting_is_active(
            &MeetingPlatform::Slack,
            &disabled,
            true
        ));
        let mut hidden = nodes.clone();
        hidden[index].bounds = None;
        assert_eq!(slack_huddle_recording_label(&hidden), None);
        assert!(!scoped_meeting_is_active(
            &MeetingPlatform::Slack,
            &hidden,
            true
        ));
    }
    let mut split = nodes.clone();
    split.insert(3, node(4, "AXWindow", "Another Slack window", None));
    assert_eq!(slack_huddle_recording_label(&split), None);
    assert!(!scoped_meeting_is_active(
        &MeetingPlatform::Slack,
        &split,
        true
    ));
    assert_eq!(slack_huddle_recording_label(&nodes[..1]), None);
}

#[test]
fn test_slack_huddle_requires_huddle_label_and_enabled_leave_control() {
    let mut composer = node(2, "AXTextArea", "Message to test", None);
    composer.settable_value = true;
    let mut disabled_leave = node(1, "AXButton", "Leave Huddle", None);
    disabled_leave.enabled = Some(false);

    assert_eq!(slack_huddle_context(&[composer.clone()]), None);
    assert_eq!(
        slack_huddle_context(&[node(0, "AXWindow", "Huddle in test", None), disabled_leave,]),
        None
    );
    assert_eq!(
        slack_huddle_context(&[
            node(0, "AXWindow", "Huddle in  test", None),
            node(1, "AXButton", "Leave Huddle", None),
            composer,
        ]),
        Some(("Huddle in  test".to_string(), "test".to_string()))
    );
}

#[test]
fn test_slack_huddle_rejects_channel_window_title_without_huddle_identity() {
    assert_eq!(
        slack_huddle_context(&[
            node(0, "AXWindow", "test - Fastrepl - Slack", None),
            node(1, "AXToolbar", "Huddles actions", None),
            node(2, "AXButton", "Leave Huddle", None),
        ]),
        None
    );

    assert_eq!(
        slack_huddle_context(&[
            node(0, "AXWindow", "test - Fastrepl - Slack", None),
            node(1, "AXButton", "Leave Huddle", None),
        ]),
        None
    );
}

#[test]
fn test_ordinary_slack_composer_is_not_a_huddle_composer() {
    let mut composer = node(2, "AXTextArea", "Message #general", None);
    composer.settable_value = true;

    assert!(!is_slack_huddle_composer(&composer, "test"));
    assert!(candidate_chat_target(&composer).is_none());
    assert_eq!(slack_huddle_context(&[composer]), None);
}

#[test]
fn test_slack_hidden_thread_control_is_recognized() {
    let control = node(3, "AXButton", "Show/hide Thread", None);

    assert!(is_slack_thread_control(&control));
    assert_eq!(
        candidate_chat_target(&control).unwrap().kind,
        "openChatControl"
    );
}

#[test]
fn test_slack_composer_and_send_button_must_share_live_thread_container() {
    let mut composer = node(4, "AXTextArea", "Message to test", None);
    composer.settable_value = true;
    let thread = [
        ancestor("Thread in test (private channel)"),
        ancestor("composer"),
    ];
    let other_thread = [ancestor("Thread in random (private channel)")];
    let duplicate_label_other_path = [ancestor_at("Thread in test (private channel)", &[9, 4])];

    assert!(is_slack_huddle_composer_in_thread(
        &composer, &thread, "test"
    ));
    assert!(!is_slack_huddle_composer_in_thread(&composer, &[], "test"));

    let mut send = node(5, "AXButton", "Send now", None);
    send.enabled = Some(false);
    assert!(!is_slack_send_now_in_thread(&send, &thread, "test", &[0]));

    send.enabled = Some(true);
    assert!(is_slack_send_now_in_thread(&send, &thread, "test", &[0]));
    assert!(!is_slack_send_now_in_thread(
        &send,
        &other_thread,
        "test",
        &[0]
    ));
    assert!(!is_slack_send_now_in_thread(
        &send,
        &duplicate_label_other_path,
        "test",
        &[0]
    ));
}

#[test]
fn test_slack_composer_selection_fails_on_ambiguity_and_drafts() {
    let mut first = node(4, "AXTextArea", "Message to test", None);
    first.settable_value = true;
    let mut second = first.clone();
    second.index = 5;

    assert_eq!(
        unique_matching_index([&first, &second].into_iter().enumerate(), |node| {
            is_slack_huddle_composer(node, "test")
        },),
        UniqueMatch::Ambiguous
    );

    first.value = Some("existing draft".to_string());
    assert!(has_nonempty_draft(&first));
    first.value = Some(" \n ".to_string());
    assert!(!has_nonempty_draft(&first));
    assert!(chat_input_is_owned("disclosure", "disclosure"));
    assert!(!chat_input_is_owned(
        "disclosure plus user text",
        "disclosure"
    ));
}
