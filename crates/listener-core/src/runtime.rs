use crate::events::*;

pub trait ListenerRuntime: anlg_storage::StorageRuntime {
    fn capture_allowed(&self, _session_id: &str) -> bool {
        true
    }
    fn observe_raw_audio_frames(&self, _session_id: &str, _frames: u64, _captured: bool) {}
    fn capture_source_ended(&self, _session_id: &str) {}
    fn emit_lifecycle(&self, event: SessionLifecycleEvent);
    fn emit_progress(&self, event: SessionProgressEvent);
    fn emit_error(&self, event: SessionErrorEvent);
    fn emit_data(&self, event: SessionDataEvent);
}
