use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingOccurrence {
    pub key: String,
    pub room: Option<String>,
    pub kind: OccurrenceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum OccurrenceKind {
    Calendar,
    Room,
    Session,
    Ambiguous,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum MeetingRecordingState {
    Recording,
    Excluded,
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRecordingContext {
    pub session_id: String,
    pub user_id: String,
    pub occurrence: MeetingOccurrence,
    pub revision: String,
    pub state: MeetingRecordingState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingRecordingAction {
    Stop,
    Resume,
}

type Handler = Arc<dyn Fn(MeetingRecordingAction, MeetingRecordingContext) + Send + Sync>;

#[derive(Default)]
struct Control {
    context: Option<MeetingRecordingContext>,
    pending: bool,
    uncertain: bool,
    handler: Option<Handler>,
}
static CONTROL: Mutex<Control> = Mutex::new(Control {
    context: None,
    pending: false,
    uncertain: false,
    handler: None,
});

pub fn set_meeting_recording_handler(handler: Handler) {
    CONTROL.lock().unwrap_or_else(|e| e.into_inner()).handler = Some(handler);
}

pub fn set_meeting_recording_context(
    app: &tauri::AppHandle,
    context: Option<MeetingRecordingContext>,
) -> tauri::Result<()> {
    {
        let mut control = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        control.context = context;
        control.pending = false;
        control.uncertain = false;
    }
    use crate::TrayPluginExt;
    app.tray().refresh_meeting_control()
}

pub fn set_meeting_recording_error(
    app: &tauri::AppHandle,
    mut context: MeetingRecordingContext,
) -> tauri::Result<()> {
    context.state = MeetingRecordingState::Pending;
    {
        let mut control = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
        control.context = Some(context);
        control.pending = false;
        control.uncertain = true;
    }
    use crate::TrayPluginExt;
    app.tray().refresh_meeting_control()
}

pub(crate) fn menu_context() -> (Option<MeetingRecordingContext>, bool, bool) {
    let control = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    (control.context.clone(), control.pending, control.uncertain)
}

fn selected_action(
    control: &mut Control,
    id: &str,
) -> Option<(Handler, MeetingRecordingAction, MeetingRecordingContext)> {
    let context = control.context.as_ref()?;
    let (action, revision) = if let Some(revision) = id.strip_prefix("e3_meeting_stop:") {
        (MeetingRecordingAction::Stop, revision)
    } else {
        (
            MeetingRecordingAction::Resume,
            id.strip_prefix("e3_meeting_resume:")?,
        )
    };
    if control.pending
        || context.revision != revision
        || (action == MeetingRecordingAction::Resume)
            != (context.state == MeetingRecordingState::Excluded)
    {
        return None;
    }
    let handler = control.handler.clone()?;
    let context = context.clone();
    control.pending = true;
    Some((handler, action, context))
}

pub(crate) fn handle_meeting_recording_menu(id: &str) {
    let selected = selected_action(&mut CONTROL.lock().unwrap_or_else(|e| e.into_inner()), id);
    if let Some((handler, action, context)) = selected {
        // Never invoke another plugin while holding the tray state lock.
        handler(action, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_or_repeated_menu_selection_cannot_stop_the_replacement_call() {
        let context = MeetingRecordingContext {
            session_id: "new-session".into(),
            user_id: "user".into(),
            occurrence: MeetingOccurrence {
                key: "new-occurrence".into(),
                room: None,
                kind: OccurrenceKind::Session,
                event_id: None,
            },
            revision: "new-revision".into(),
            state: MeetingRecordingState::Recording,
        };
        let mut control = Control {
            context: Some(context.clone()),
            handler: Some(Arc::new(|_, _| {})),
            pending: false,
            uncertain: false,
        };
        assert!(selected_action(&mut control, "e3_meeting_stop:old-revision").is_none());
        let (_, action, selected) =
            selected_action(&mut control, "e3_meeting_stop:new-revision").unwrap();
        assert_eq!(action, MeetingRecordingAction::Stop);
        assert_eq!(selected, context);
        assert!(selected_action(&mut control, "e3_meeting_stop:new-revision").is_none());
    }
}
