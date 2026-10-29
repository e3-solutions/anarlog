const COMMANDS: &[&str] = &[
    "recording_diagnostics_configure",
    "recording_diagnostics_record",
    "recording_diagnostics_batch",
    "recording_diagnostics_ack",
    "shared_recording_configuration",
    "meeting_recording_control_set",
    "meeting_recording_exclusions",
    "shared_recording_configure",
    "shared_recording_observe",
    "shared_recording_enqueue",
    "shared_recording_queue",
    "shared_recording_flush",
    "list_microphone_devices",
    "get_current_microphone_device",
    "get_mic_muted",
    "set_mic_muted",
    "start_capture",
    "retry_audio_capture",
    "flush_live_transcript",
    "release_live_transcript",
    "list_stopped_captures",
    "get_stopped_capture",
    "acknowledge_stopped_capture",
    "save_capture_lifecycle_marker",
    "clear_capture_lifecycle_marker",
    "get_capture_lifecycle_marker",
    "list_capture_lifecycle_markers",
    "list_capture_recoveries",
    "get_capture_audio_gaps",
    "mark_capture_audio_saved",
    "clear_capture_audio_saved",
    "stop_capture",
    "stop_capture_for_session",
    "get_capture_state",
    "get_capture_snapshot",
    "update_capture_credentials",
    "list_capture_audio_chunks",
    "get_capture_audio_cleanup_status",
    "acknowledge_capture_audio_cleanup_status",
    "acknowledge_capture_audio_chunk",
    "delete_transcribed_capture_audio",
    "is_supported_languages_live",
    "suggest_providers_for_languages_live",
    "list_documented_language_codes_live",
    "render_transcript_segments",
    "render_session_transcript",
    "start_transcription",
    "stop_transcription",
    "list_transcription_sessions",
    "get_completed_transcription",
    "acknowledge_completed_transcription",
    "extract_voiceprint_candidates",
    "promote_voiceprint_candidates",
    "delete_processed_session_audio",
    "parse_subtitle",
    "export_to_vtt",
    "is_supported_languages_batch",
    "suggest_providers_for_languages_batch",
    "list_documented_language_codes_batch",
    "refine_batch_transcript",
    "save_batch_transcript",
    "reconcile_refined_speaker_clusters",
    "extract_voiceprint_candidates",
    "promote_voiceprint_candidates",
];

fn main() {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|sha| sha.trim().to_string())
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ANLG_DIAGNOSTICS_BUILD_SHA={sha}");
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD"])
        .output()
        && let Ok(path) = String::from_utf8(output.stdout)
    {
        println!("cargo:rerun-if-changed={}", path.trim());
    }
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--git-path", "refs"])
        .output()
        && let Ok(path) = String::from_utf8(output.stdout)
    {
        println!("cargo:rerun-if-changed={}", path.trim());
    }
    tauri_plugin::Builder::new(COMMANDS).build();
}
