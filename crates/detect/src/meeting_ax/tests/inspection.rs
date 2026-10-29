use super::*;

fn meet_window(code: &str, window_hash: usize, joined: bool) -> BrowserMeetingRoot {
    let mut window = fixture_node(0, "AXWindow", "Google Meet", &[]);
    window.element_hash = Some(window_hash);
    BrowserMeetingRoot {
        platform: MeetingPlatform::GoogleMeet,
        window_title: Some("Google Meet".to_string()),
        web_area_url: Some(format!("https://meet.google.com/{code}")),
        nodes: vec![
            window,
            fixture_node(
                1,
                "AXButton",
                if joined { "Leave call" } else { "Rejoin" },
                &[0],
            ),
        ],
    }
}

fn meet_home() -> BrowserMeetingRoot {
    let mut home = meet_window("", 0x2000, false);
    home.nodes.truncate(1);
    home
}

fn sharing_meet_window() -> BrowserMeetingRoot {
    let mut root = meet_window("abc-defg-hij", 0x1000, true);
    root.nodes.push(fixture_node(
        2,
        "AXStaticText",
        "Alex Kim is presenting",
        &[1],
    ));
    assert!(nodes_show_remote_screen_share(&root.nodes));
    root
}

fn browser_rows(roots: Vec<BrowserMeetingRoot>) -> Vec<MeetingAccessibilityInspection> {
    browser_inspections_from_roots(
        MeetingApp {
            id: "com.google.Chrome".to_string(),
            name: "Chrome".to_string(),
        },
        123,
        true,
        roots,
        false,
        Vec::new(),
    )
}

#[test]
fn browser_inspection_reports_join_and_departure_beside_home_without_ambiguity() {
    let joined = browser_rows(vec![meet_home(), sharing_meet_window()]);
    assert_eq!(joined.len(), 2);
    assert!(!joined[0].active_call);
    assert!(joined[0].call_identity.is_none());
    assert!(joined[1].active_call);
    assert!(!joined[0].remote_screen_share);
    assert!(joined[1].remote_screen_share);
    let identity = joined[1].call_identity.clone().unwrap();
    let departed = browser_rows(vec![
        meet_home(),
        meet_window("abc-defg-hij", 0x1000, false),
    ]);
    assert!(!departed[1].active_call);
    assert_eq!(departed[1].call_identity.as_ref(), Some(&identity));
    assert!(departed[1].warnings.is_empty());
}

#[test]
fn browser_inspection_recognizes_prefixed_meet_routes_and_binds_the_same_chrome_tab() {
    for url in [
        "https://meet.google.com/abc-defg-hij",
        "https://meet.google.com/meet/abc-defg-hij?authuser=0",
        "https://meet.google.com/meet/abc-defg-hij/?authuser=1#details",
    ] {
        let mut root = meet_window("abc-defg-hij", 0x1000, true);
        root.web_area_url = Some(url.to_string());
        let rows = browser_rows(vec![root]);
        assert!(rows[0].active_call);
        assert_eq!(
            rows[0].call_identity.as_deref(),
            Some("https://meet.google.com/abc-defg-hij:window:1000")
        );
        let inventory =
            chrome_tabs::parse_inventory(&format!("E3_MEET_TABS_V1\n42\t{url}\nE3_MEET_TABS_END"))
                .unwrap();
        assert_eq!(
            chrome_tabs::unique_identity("https://meet.google.com/abc-defg-hij", &inventory)
                .as_deref(),
            Some("https://meet.google.com/abc-defg-hij:tab:42")
        );
    }
}

#[test]
fn browser_inspection_identity_is_stable_across_url_query_but_not_other_calls_or_windows() {
    let a = meet_window("abc-defg-hij", 0x1000, true);
    let mut refreshed = meet_window("abc-defg-hij", 0x1000, false);
    refreshed.web_area_url =
        Some("https://meet.google.com/abc-defg-hij?authuser=1#details".to_string());
    assert_eq!(browser_call_identity(&a), browser_call_identity(&refreshed));
    assert_ne!(
        browser_call_identity(&a),
        browser_call_identity(&meet_window("abc-defg-hij", 0x3000, false))
    );
    assert_ne!(
        browser_call_identity(&a),
        browser_call_identity(&meet_window("xyz-abcd-efg", 0x1000, false))
    );
}

#[test]
fn browser_inspection_has_no_sticky_cache_when_a_is_hidden_or_closed_and_b_starts() {
    let a = browser_rows(vec![meet_window("abc-defg-hij", 0x1000, true)]);
    let home = browser_rows(vec![meet_home()]);
    assert!(home[0].call_identity.is_none());
    let b = browser_rows(vec![meet_window("xyz-abcd-efg", 0x3000, true)]);
    assert!(b[0].active_call);
    assert_ne!(a[0].call_identity, b[0].call_identity);
    let ended_b = browser_rows(vec![meet_window("xyz-abcd-efg", 0x3000, false)]);
    assert_eq!(b[0].call_identity, ended_b[0].call_identity);
    assert_ne!(a[0].call_identity, ended_b[0].call_identity);
}

#[test]
fn browser_inspection_blocks_competing_calls_including_browser_specific_teams_controls() {
    let teams = BrowserMeetingRoot {
        platform: MeetingPlatform::MicrosoftTeams,
        window_title: Some("Microsoft Teams".to_string()),
        web_area_url: Some("https://teams.microsoft.com/meet/123".to_string()),
        nodes: vec![fixture_node(0, "AXButton", "Leave", &[0])],
    };
    let rows = browser_rows(vec![sharing_meet_window(), teams]);
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| !row.active_call && !row.remote_screen_share)
    );
    assert!(rows.iter().all(|row| {
        row.warnings
            .iter()
            .any(|warning| warning.contains("ambiguous"))
    }));
}

#[test]
fn browser_inspection_preserves_process_presence_when_contexts_unavailable() {
    let rows = browser_rows(Vec::new());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pid, 123);
    assert_eq!(rows[0].app.id, "com.google.Chrome");
    assert!(!rows[0].active_call);
    assert!(rows[0].call_identity.is_none());
    assert!(
        rows[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("no uniquely scoped"))
    );
}

#[test]
fn browser_inspection_incomplete_windows_never_emit_active_or_remote_share() {
    let rows = browser_inspections_from_roots(
        MeetingApp {
            id: "com.google.Chrome".to_string(),
            name: "Chrome".to_string(),
        },
        123,
        true,
        vec![sharing_meet_window()],
        true,
        Vec::new(),
    );
    assert!(!rows[0].active_call && !rows[0].remote_screen_share);
    assert!(
        rows[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("ambiguous"))
    );
}

#[test]
fn browser_inspection_accepts_actual_left_heading_with_unlabelled_buttons() {
    let joined = browser_rows(vec![meet_window("abc-defg-hij", 0x1000, true)]);
    let mut ended = meet_window("abc-defg-hij", 0x1000, false);
    ended.nodes.truncate(1);
    ended.nodes.extend([
        fixture_node(1, "AXHeading", "You left the meeting", &[0]),
        fixture_node(2, "AXButton", "", &[1]),
        fixture_node(3, "AXButton", "", &[2]),
        fixture_node(4, "AXButton", "", &[3]),
    ]);
    assert!(node_needs_bounds(
        &Some("AXHeading".to_string()),
        false,
        Some("You left the meeting")
    ));
    let rows = browser_rows(vec![ended]);
    assert!(!rows[0].active_call);
    assert_eq!(rows[0].call_identity, joined[0].call_identity);
    assert!(rows[0].warnings.is_empty());
}

#[test]
fn browser_inspection_recognizes_host_ended_heading_without_a_rejoin_button() {
    let joined = browser_rows(vec![meet_window("abc-defg-hij", 0x1000, true)]);
    let mut ended = meet_window("abc-defg-hij", 0x1000, false);
    ended.nodes.truncate(1);
    ended.nodes.extend([
        fixture_node(
            1,
            "AXHeading",
            "You've ended the meeting for everyone",
            &[0],
        ),
        fixture_node(2, "AXButton", "", &[1]),
        fixture_node(3, "AXButton", "", &[2]),
    ]);
    let rows = browser_rows(vec![ended]);
    assert!(!rows[0].active_call);
    assert_eq!(rows[0].call_identity, joined[0].call_identity);
    assert!(rows[0].warnings.is_empty());
}

#[test]
fn browser_inspection_hidden_departure_or_missing_window_identity_remain_unknown() {
    let mut hidden = meet_window("abc-defg-hij", 0x1000, false);
    hidden.nodes[1].bounds = None;
    let mut disabled = meet_window("abc-defg-hij", 0x1000, false);
    disabled.nodes[1].enabled = Some(false);
    let mut no_hash = meet_window("abc-defg-hij", 0x1000, true);
    no_hash.nodes[0].element_hash = None;
    for root in [hidden, disabled, no_hash] {
        let rows = browser_rows(vec![root]);
        assert!(!rows[0].active_call);
        assert!(rows[0].call_identity.is_none());
        assert!(
            rows[0]
                .warnings
                .iter()
                .any(|warning| warning.contains("incomplete"))
        );
    }
}

#[test]
fn test_bounds_are_only_requested_for_visibility_sensitive_nodes() {
    assert!(node_needs_bounds(
        &Some("AXHeading".to_string()),
        false,
        Some("You left the meeting")
    ));
    assert!(!node_needs_bounds(
        &Some("AXStaticText".to_string()),
        false,
        Some("meeting transcript"),
    ));
    assert!(node_needs_bounds(
        &Some("AXButton".to_string()),
        false,
        Some("leave call"),
    ));
    assert!(node_needs_bounds(&Some("AXGroup".to_string()), true, None,));
    assert!(node_needs_bounds(
        &Some("AXStaticText".to_string()),
        false,
        Some("Write a message to everyone, press Shift + Enter for new line"),
    ));
}

#[test]
fn test_chat_inspection_does_not_use_writable_value_as_label() {
    let mut input = node(6, "AXTextArea", "", None);
    input.title = None;
    input.settable_value = true;
    input.value = Some("private draft".to_string());
    input.text = searchable_node_text(
        &input.role,
        &input.title,
        &input.value,
        &input.description,
        &input.placeholder,
        input.settable_value,
    );

    assert!(!input.text.contains("private draft"));
    assert!(candidate_chat_target(&input).is_none());
    assert_eq!(inspection_label(&input), None);

    let mut read_only_input = input.clone();
    read_only_input.settable_value = false;
    read_only_input.value = Some("private read-only text".to_string());
    read_only_input.text = searchable_node_text(
        &read_only_input.role,
        &read_only_input.title,
        &read_only_input.value,
        &read_only_input.description,
        &read_only_input.placeholder,
        read_only_input.settable_value,
    );
    assert!(!read_only_input.text.contains("private read-only text"));
    assert_eq!(node_labels(&read_only_input).count(), 0);

    let mut secure_input = read_only_input.clone();
    secure_input.role = Some("AXSecureTextField".to_string());
    secure_input.value = Some("private password".to_string());
    secure_input.text = searchable_node_text(
        &secure_input.role,
        &secure_input.title,
        &secure_input.value,
        &secure_input.description,
        &secure_input.placeholder,
        secure_input.settable_value,
    );
    assert!(!secure_input.text.contains("private password"));
    assert_eq!(node_labels(&secure_input).count(), 0);
}

#[test]
fn test_native_meeting_window_validation_is_evidence_backed() {
    let settings = [
        node(0, "AXWindow", "Zoom Workplace Settings", None),
        node(1, "AXStaticText", "Video", None),
        node(2, "AXButton", "Camera preview", None),
    ];
    let zoom_participant_tile = [node(
        3,
        "AXGroup",
        "Video render Ada Lovelace, Computer audio unmuted",
        None,
    )];
    let discord_voice = [node(4, "AXStaticText", "Voice connected", None)];

    assert!(!native_meeting_window_is_validated(
        &MeetingPlatform::Zoom,
        &settings,
    ));
    assert!(!native_meeting_window_is_validated(
        &MeetingPlatform::Zoom,
        &zoom_participant_tile,
    ));
    let current_linux_zoom = [
        node(4, "AXWindow", "Meeting", None),
        node(
            5,
            "AXButton",
            "Leave",
            Some(AxRect {
                x: 1224.0,
                y: 792.0,
                width: 78.0,
                height: 56.0,
            }),
        ),
    ];
    assert!(native_meeting_window_is_validated(
        &MeetingPlatform::Zoom,
        &current_linux_zoom,
    ));
    assert!(!native_meeting_window_is_validated(
        &MeetingPlatform::Zoom,
        &[node(6, "AXWindow", "Meeting", None)],
    ));
    assert!(!native_meeting_window_is_validated(
        &MeetingPlatform::Zoom,
        &[
            node(7, "AXWindow", "Leave meeting", None),
            node(
                8,
                "AXButton",
                "Leave",
                Some(AxRect {
                    x: 898.0,
                    y: 588.0,
                    width: 70.0,
                    height: 32.0,
                }),
            ),
        ],
    ));
    assert!(!native_meeting_window_is_validated(
        &MeetingPlatform::Zoom,
        &[node(
            9,
            "AXTabGroup",
            "John Jeong, Computer audio muted",
            None,
        )],
    ));
    assert!(native_meeting_window_is_validated(
        &MeetingPlatform::Discord,
        &discord_voice,
    ));
    for platform in [MeetingPlatform::MicrosoftTeams, MeetingPlatform::Webex] {
        assert!(!native_meeting_window_is_validated(
            &platform,
            &zoom_participant_tile,
        ));
    }
    assert!(native_meeting_window_is_validated(
        &MeetingPlatform::MicrosoftTeams,
        &[fixture_node(5, "AXButton", "Hang up", &[0])],
    ));
    assert!(!native_meeting_window_is_validated(
        &MeetingPlatform::MicrosoftTeams,
        &[fixture_node(6, "AXButton", "Leave", &[0])],
    ));
    assert!(native_meeting_window_is_validated(
        &MeetingPlatform::MicrosoftTeams,
        &[
            fixture_node(6, "AXButton", "Leave", &[0]),
            fixture_node(7, "AXToolbar", "Meeting controls", &[0]),
        ],
    ));
    assert!(native_meeting_window_is_validated(
        &MeetingPlatform::Webex,
        &[fixture_node(8, "AXButton", "Leave meeting", &[0])],
    ));
}

#[test]
fn test_native_fallback_evidence_marks_calls_active() {
    let discord_voice = [node(0, "AXStaticText", "Voice connected", None)];
    let teams_fallback = [
        fixture_node(1, "AXButton", "Leave", &[0]),
        fixture_node(2, "AXToolbar", "Meeting controls", &[0]),
    ];

    assert!(scoped_meeting_is_active(
        &MeetingPlatform::Discord,
        &discord_voice,
        true,
    ));
    assert!(scoped_meeting_is_active(
        &MeetingPlatform::MicrosoftTeams,
        &teams_fallback,
        true,
    ));
    assert!(!scoped_meeting_is_active(
        &MeetingPlatform::Discord,
        &discord_voice,
        false,
    ));
}

#[test]
fn test_remote_screen_share_ignores_own_share_and_share_controls() {
    for label in [
        "Alex Kim is presenting",
        "You are viewing Alex Kim's screen",
        "Priya is sharing their screen",
        "Alex Kim (Presentation)",
        "Alex Kim's screen, selected",
    ] {
        assert!(label_indicates_remote_screen_share(label), "{label}");
    }
    for label in [
        "You are presenting",
        "You're sharing your screen",
        "Stop presenting",
        "Share screen",
        "Present now",
        "Change screen",
        "Your screen",
    ] {
        assert!(!label_indicates_remote_screen_share(label), "{label}");
    }
}

#[test]
fn browser_inspection_accepts_two_account_views_only_for_the_same_known_meet() {
    for (second, expected) in [
        (meet_window("abc-defg-hij", 0x3000, true), true),
        (meet_window("xyz-abcd-efg", 0x3000, true), false),
        (meet_window("invalid", 0x3000, true), false),
    ] {
        let rows = browser_rows(vec![sharing_meet_window(), second]);
        assert!(rows.iter().all(|row| row.active_call == expected));
    }
}
