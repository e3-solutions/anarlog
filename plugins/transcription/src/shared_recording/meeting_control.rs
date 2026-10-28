use super::*;
use tauri::Emitter;
use tauri_plugin_tray::{
    MeetingOccurrence, MeetingRecordingAction, MeetingRecordingContext, MeetingRecordingState,
    OccurrenceKind,
};

const CONTROL_EVENT: &str = "e3:meeting-recording-control";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hold {
    context: MeetingRecordingContext,
    active: bool,
    #[serde(default)]
    generation: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlEvent {
    action: &'static str,
    context: MeetingRecordingContext,
    error: Option<String>,
}

fn control_path(dir: &Path) -> PathBuf {
    dir.join("meeting-recording-control.json")
}
fn hold_path(dir: &Path, revision: &str) -> PathBuf {
    dir.join(format!("meeting-hold-{}.json", key(revision)))
}
fn hold_records(dir: &Path) -> Result<Vec<Hold>, String> {
    let mut holds = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("meeting-hold-") && name.ends_with(".json") {
            if let Some(hold) = read::<Hold>(&entry.path())? {
                holds.push(hold);
            }
        }
    }
    Ok(holds)
}
#[cfg(test)]
fn held_contexts(dir: &Path, user: &str) -> Result<Vec<MeetingRecordingContext>, String> {
    Ok(hold_records(dir)?
        .into_iter()
        .filter(|hold| hold.active && hold.context.user_id == user)
        .map(|hold| hold.context)
        .collect())
}
fn next_generation(dir: &Path) -> Result<u64, String> {
    // Include released holds so an older Resume never outranks a later Stop after restart.
    hold_records(dir)?
        .iter()
        .map(|hold| hold.generation)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "Recording hold sequence is exhausted".into())
}
fn live_source(
    state: &QueueState,
    current: Option<&MeetingRecordingContext>,
    except_session: Option<&str>,
) -> bool {
    use anlg_transcription_core::listener::actors::SourceActor;
    let mut sessions = state
        .raw
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    if let Some(current) = current {
        sessions.push(current.session_id.clone());
    }
    sessions.into_iter().any(|session| {
        except_session != Some(session.as_str())
            && ractor::registry::where_is(SourceActor::name(&session)).is_some()
    })
}
fn active_holds<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    user: &str,
) -> Result<Vec<MeetingRecordingContext>, String> {
    let state = app.state::<QueueState>();
    let memory = state
        .holds
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|context| context.user_id == user)
        .cloned()
        .collect::<Vec<_>>();
    let disk = directory(app).and_then(|dir| hold_records(&dir));
    let records = match disk {
        Ok(records) => records,
        Err(_) if !memory.is_empty() => Vec::new(),
        Err(error) => return Err(error),
    };
    let generations = records
        .iter()
        .map(|hold| (hold.context.revision.clone(), hold.generation))
        .collect::<HashMap<_, _>>();
    let mut contexts = records
        .into_iter()
        .filter(|hold| hold.active && hold.context.user_id == user)
        .map(|hold| hold.context)
        .collect::<Vec<_>>();
    for context in memory {
        contexts.retain(|held| held.revision != context.revision);
        contexts.push(context);
    }
    let current_revision = state
        .control
        .try_lock()
        .ok()
        .and_then(|current| current.as_ref().map(|context| context.revision.clone()));
    contexts.sort_by_key(|context| {
        std::cmp::Reverse((
            generations
                .get(&context.revision)
                .copied()
                .unwrap_or(u64::MAX),
            current_revision.as_ref() == Some(&context.revision),
        ))
    });
    Ok(contexts)
}

fn overlaps(held: &MeetingRecordingContext, next: &MeetingRecordingContext) -> bool {
    if held.user_id != next.user_id {
        return false;
    }
    if held.occurrence.key == next.occurrence.key
        || (held.occurrence.kind == OccurrenceKind::Ambiguous && held.occurrence.room.is_none())
    {
        return true;
    }
    if next.occurrence.kind == OccurrenceKind::Calendar {
        held.occurrence.kind != OccurrenceKind::Calendar
            && next.occurrence.room.is_some()
            && held.occurrence.room == next.occurrence.room
    } else {
        next.occurrence
            .room
            .as_ref()
            .map_or(next.occurrence.kind == OccurrenceKind::Ambiguous, |room| {
                held.occurrence.room.as_ref() == Some(room)
            })
    }
}
fn resumable(held: &MeetingRecordingContext, next: &MeetingRecordingContext) -> bool {
    held.user_id == next.user_id
        && (held.occurrence.key == next.occurrence.key
            || (held.occurrence.kind == OccurrenceKind::Ambiguous
                && held.occurrence.room.is_none())
            || (held.occurrence.kind != OccurrenceKind::Calendar
                && next.occurrence.room.is_some()
                && held.occurrence.room == next.occurrence.room))
}
fn fallback(session: &str, user: &str) -> MeetingRecordingContext {
    MeetingRecordingContext {
        session_id: session.into(),
        user_id: user.into(),
        occurrence: MeetingOccurrence {
            key: format!("session:{session}"),
            room: None,
            kind: OccurrenceKind::Ambiguous,
            event_id: None,
        },
        revision: uuid::Uuid::new_v4().to_string(),
        state: MeetingRecordingState::Pending,
    }
}

pub(crate) fn prepare_capture<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: &str,
    user: Option<&str>,
) -> Result<(), String> {
    let state = app.state::<QueueState>();
    let dir = directory(app)?;
    let user = user.unwrap_or("local-device");
    let mut current = state.control.lock().unwrap_or_else(|e| e.into_inner());
    if live_source(&state, current.as_ref(), None) {
        return Err("Another recording is still active".into());
    }
    let context = current
        .clone()
        .or(read::<Option<MeetingRecordingContext>>(&control_path(&dir))?.flatten())
        .filter(|context: &MeetingRecordingContext| {
            context.session_id == session && context.user_id == user
        })
        .unwrap_or_else(|| fallback(session, user));
    if active_holds(app, user)?
        .iter()
        .any(|held| overlaps(held, &context))
    {
        return Err("This meeting is excluded. Resume recording intentionally first.".into());
    }
    save(&control_path(&dir), &context)?;
    *current = Some(context);
    Ok(())
}

pub(crate) fn hold_for_session<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: &str,
    user: &str,
) -> Result<MeetingRecordingContext, String> {
    let state = app.state::<QueueState>();
    let mut current = state.control.lock().unwrap_or_else(|e| e.into_inner());
    let mut context = current
        .clone()
        .filter(|context: &MeetingRecordingContext| {
            context.session_id == session && context.user_id == user
        })
        .unwrap_or_else(|| fallback(session, user));
    context.state = MeetingRecordingState::Excluded;
    if context.occurrence.kind == OccurrenceKind::Session {
        context.occurrence.kind = OccurrenceKind::Ambiguous;
    }
    // Retain the hold in memory even if persistence fails.
    state
        .holds
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(context.revision.clone(), context.clone());
    let mut stopping = context.clone();
    stopping.state = MeetingRecordingState::Pending;
    *current = Some(stopping);
    state
        .hold_errors
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(context.revision.clone());
    let dir = directory(app)?;
    let existing = read::<Hold>(&hold_path(&dir, &context.revision))?;
    if existing.as_ref().is_some_and(|hold| {
        hold.context.user_id != context.user_id
            || hold.context.session_id != context.session_id
            || hold.context.occurrence != context.occurrence
    }) {
        return Err("Recording hold revision belongs to another context".into());
    }
    let generation = match existing.filter(|hold| hold.active) {
        Some(hold) => hold.generation,
        None => next_generation(&dir)?,
    };
    save(
        &hold_path(&dir, &context.revision),
        &Hold {
            context: context.clone(),
            active: true,
            generation,
        },
    )?;
    save(&control_path(&dir), &context)?;
    state
        .hold_errors
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&context.revision);
    Ok(context)
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_recording_exclusions(
    app: tauri::AppHandle,
    user_id: String,
) -> Result<Vec<MeetingRecordingContext>, String> {
    active_holds(&app, &user_id)
}

fn set_context<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    context: Option<MeetingRecordingContext>,
    resume_of: Option<String>,
) -> Result<Option<MeetingRecordingContext>, String> {
    let state = app.state::<QueueState>();
    let mut current = state.control.lock().unwrap_or_else(|e| e.into_inner());
    if context.is_none()
        && current
            .as_ref()
            .is_some_and(|context| context.state == MeetingRecordingState::Recording)
    {
        return Err("Stop the current recording before clearing its control".into());
    }
    if let Some(context) = context.as_ref() {
        if live_source(&state, current.as_ref(), Some(&context.session_id)) {
            return Err("Another recording is still active".into());
        }
        let active_other_context = current.as_ref().is_some_and(|active| {
            active.state == MeetingRecordingState::Recording
                && (active.session_id != context.session_id || active.user_id != context.user_id)
        });
        let other_owner = state
            .raw
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&context.session_id)
            .and_then(|timeline| timeline.capture_user_id.as_deref())
            .is_some_and(|owner| owner != context.user_id);
        if active_other_context || other_owner {
            return Err("Another recording is still active".into());
        }
        if context.user_id.is_empty()
            || context.session_id.is_empty()
            || context.revision.is_empty()
            || context.occurrence.key.is_empty()
        {
            return Err(
                "Recording context requires an account, session, occurrence and revision".into(),
            );
        }
        if current.as_ref().is_some_and(|previous| {
            previous.revision == context.revision
                && (previous.session_id != context.session_id
                    || previous.user_id != context.user_id
                    || previous.occurrence != context.occurrence)
        }) {
            return Err("Use a fresh recording revision when the meeting changes".into());
        }
        if context.state != MeetingRecordingState::Excluded {
            ensure_allowed(app, &context.session_id)?;
            let holds = active_holds(app, &context.user_id)?;
            let resuming = resume_of
                .as_ref()
                .and_then(|revision| holds.iter().find(|held| &held.revision == revision));
            if holds.iter().any(|held| {
                overlaps(held, context) && resuming.is_none_or(|_| !resumable(held, context))
            }) {
                return Err(
                    "This meeting is excluded. Resume recording intentionally first.".into(),
                );
            }
        }
    }
    let release = if let Some(revision) = resume_of.as_deref() {
        let context = context
            .as_ref()
            .ok_or("Resume needs a new recording context")?;
        let holds = active_holds(app, &context.user_id)?;
        let held = holds
            .iter()
            .find(|held| held.revision == revision)
            .ok_or("This recording hold is no longer available")?;
        if live_source(&state, current.as_ref(), None)
            || current
                .as_ref()
                .is_some_and(|active| active.state == MeetingRecordingState::Recording)
        {
            return Err("Recording context changed. Resume the current meeting again.".into());
        }
        let dir = directory(app)?;
        let expected_generation =
            read::<Hold>(&hold_path(&dir, revision))?.map(|hold| hold.generation);
        for other in holds
            .iter()
            .filter(|other| other.revision != revision && resumable(other, context))
        {
            let generation =
                read::<Hold>(&hold_path(&dir, &other.revision))?.map(|hold| hold.generation);
            let latest_is_unknown = match (expected_generation, generation) {
                (Some(expected), Some(other)) => {
                    other > expected
                        || (expected == 0
                            && other == 0
                            && current
                                .as_ref()
                                .is_none_or(|active| active.revision != revision))
                }
                _ => true,
            };
            if latest_is_unknown {
                return Err(
                    "A newer recording hold may apply. Resume the latest excluded meeting again."
                        .into(),
                );
            }
        }

        if held.user_id != context.user_id
            || !resumable(held, context)
            || held.session_id == context.session_id
            || held.revision == context.revision
            || context.state != MeetingRecordingState::Pending
        {
            return Err("Recording context changed. Resume the current meeting again.".into());
        }
        Some(
            holds
                .into_iter()
                .filter(|held| resumable(held, context))
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    if context
        .as_ref()
        .is_some_and(|context| context.state == MeetingRecordingState::Excluded)
    {
        if resume_of.is_some() {
            return Err("Resume requires a fresh pending recording".into());
        }
        let mut stopping = context.clone();
        if let Some(context) = stopping.as_mut() {
            context.state = MeetingRecordingState::Pending;
        }
        *current = stopping;
        return Ok(context);
    }
    let dir = directory(app)?;
    save(&control_path(&dir), &context)?;
    if let Some(holds) = release {
        for held in &holds {
            save(
                &hold_path(&dir, &held.revision),
                &Hold {
                    context: held.clone(),
                    active: false,
                    generation: match read::<Hold>(&hold_path(&dir, &held.revision))? {
                        Some(previous) => previous.generation,
                        None => next_generation(&dir)?,
                    },
                },
            )?;
        }
        let mut memory = state.holds.lock().unwrap_or_else(|e| e.into_inner());
        for held in holds {
            memory.remove(&held.revision);
            state
                .hold_errors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&held.revision);
        }
    }
    *current = context.clone();
    Ok(context)
}

#[tauri::command]
#[specta::specta]
pub async fn meeting_recording_control_set(
    app: tauri::AppHandle,
    context: Option<MeetingRecordingContext>,
    resume_of: Option<String>,
) -> Result<Option<MeetingRecordingContext>, String> {
    let stopping = context
        .as_ref()
        .is_some_and(|context| context.state == MeetingRecordingState::Excluded);
    let context = set_context(&app, context, resume_of)?;
    if stopping {
        let context = context.as_ref().ok_or("Recording context unavailable")?;
        if let Err(error) = super::shared_recording_exclude(
            app.clone(),
            context.session_id.clone(),
            context.user_id.clone(),
        )
        .await
        {
            let current = app
                .state::<QueueState>()
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(current) = current.filter(|current| {
                current.revision == context.revision
                    && current.session_id == context.session_id
                    && current.user_id == context.user_id
            }) {
                let _ = tauri_plugin_tray::set_meeting_recording_error(&app, current);
            }
            return Err(error);
        }
    }
    let context = app
        .state::<QueueState>()
        .control
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    tauri_plugin_tray::set_meeting_recording_context(&app, context.clone())
        .map_err(|e| e.to_string())?;
    Ok(context)
}

pub(super) fn finish_stop<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    expected: &MeetingRecordingContext,
    failed: bool,
) -> Option<MeetingRecordingContext> {
    let state = app.state::<QueueState>();
    let mut current = state.control.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(active) = current.as_mut().filter(|active| {
        active.revision == expected.revision
            && active.session_id == expected.session_id
            && active.user_id == expected.user_id
    }) {
        active.state = if failed {
            MeetingRecordingState::Pending
        } else {
            MeetingRecordingState::Excluded
        };
        if let Ok(dir) = directory(app) {
            let _ = save(&control_path(&dir), active);
        }
    }
    current.clone()
}

pub(super) fn cancel_transcriptions<R: tauri::Runtime>(app: &tauri::AppHandle<R>, session: &str) {
    let Some(registry) = app.try_state::<std::sync::Arc<crate::BatchSessionRegistry>>() else {
        return;
    };
    let tokens = registry
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(id, _)| id.as_str() == session || id.starts_with(&format!("{session}:")))
        .map(|(_, entry)| entry.control.cancellation_token.clone())
        .collect::<Vec<_>>();
    for token in tokens {
        token.cancel();
    }
}

pub(super) async fn stop_verified<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session: &str,
) -> Result<(), String> {
    use anlg_transcription_core::listener::actors::{RootActor, RootMsg, SourceActor};
    use ractor::{ActorRef, registry};
    if let Some(cell) = registry::where_is(RootActor::name()) {
        let actor: ActorRef<RootMsg> = cell.into();
        ractor::call_t!(
            actor,
            RootMsg::StopSessionIfActive,
            5_000,
            session.to_string()
        )
        .map_err(|_| "Recording stop could not be confirmed".to_string())?;
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while registry::where_is(SourceActor::name(session)).is_some() {
        if tokio::time::Instant::now() >= deadline {
            return Err("Audio capture is still stopping. Retry stop for this meeting.".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    use crate::listener2::Listener2PluginExt;
    for item in app
        .listener2()
        .list_transcription_sessions()
        .map_err(|e| e.to_string())?
    {
        if item.session_id == session || item.session_id.starts_with(&format!("{session}:")) {
            app.listener2().stop_transcription(item.session_id).await;
        }
    }
    Ok(())
}

pub(crate) fn publish_runtime(app: &tauri::AppHandle, session: &str, recording: bool) {
    let state = app.state::<QueueState>();
    let mut current = state.control.lock().unwrap_or_else(|e| e.into_inner());
    if recording
        && capture_allowed(app, session)
        && current
            .as_ref()
            .is_none_or(|context| context.session_id != session)
    {
        let user = state
            .raw
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .and_then(|timeline| timeline.capture_user_id.clone())
            .unwrap_or_else(|| "local-device".into());
        *current = Some(fallback(session, &user));
    }

    if let Some(context) = current
        .as_mut()
        .filter(|context| context.session_id == session)
    {
        if context.state == MeetingRecordingState::Pending
            && state
                .holds
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&context.revision)
        {
            return;
        }

        if context.state != MeetingRecordingState::Excluded {
            context.state = if recording {
                MeetingRecordingState::Recording
            } else {
                MeetingRecordingState::Pending
            };
        }
        let context = context.clone();
        let uncertain = state
            .hold_errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&context.revision);
        if let Ok(dir) = directory(app) {
            let _ = save(&control_path(&dir), &context);
        }
        drop(current);
        if uncertain {
            let _ = tauri_plugin_tray::set_meeting_recording_error(app, context);
        } else {
            let _ = tauri_plugin_tray::set_meeting_recording_context(app, Some(context));
        }
    }
}

pub(crate) fn setup(app: &tauri::AppHandle) {
    if let Ok(dir) = directory(app) {
        if let Ok(Some(mut context)) = read::<Option<MeetingRecordingContext>>(&control_path(&dir))
        {
            if let Some(context) = context.as_mut() {
                if context.state == MeetingRecordingState::Recording {
                    context.state = MeetingRecordingState::Pending;
                }
            }
            *app.state::<QueueState>()
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = context.clone();
            let _ = tauri_plugin_tray::set_meeting_recording_context(app, context);
        }
    }
    let app = app.clone();
    tauri_plugin_tray::set_meeting_recording_handler(std::sync::Arc::new(
        move |action, expected| {
            let state = app.state::<QueueState>();
            let current = state
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if current.as_ref().is_none_or(|current| {
                current.revision != expected.revision
                    || current.session_id != expected.session_id
                    || current.user_id != expected.user_id
            }) {
                let _ = tauri_plugin_tray::set_meeting_recording_context(&app, current);
                return;
            }
            if action == MeetingRecordingAction::Resume {
                let _ = tauri_plugin_tray::set_meeting_recording_context(&app, current);
                let _ = app.emit(
                    CONTROL_EVENT,
                    ControlEvent {
                        action: "resume",
                        context: expected,
                        error: None,
                    },
                );
                return;
            }
            // Gate synchronously in the native menu handler before any frontend event.
            let result = exclude_gate(&app, &expected.session_id, &expected.user_id);
            let held = if !capture_allowed(&app, &expected.session_id) {
                hold_for_session(&app, &expected.session_id, &expected.user_id)
            } else {
                Err("Recording could not be excluded".into())
            };
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let stopped = stop_verified(&app, &expected.session_id).await;
                let stop_error = stopped.err();
                let error = stop_error
                    .clone()
                    .or_else(|| result.err())
                    .or_else(|| held.as_ref().err().cloned());
                let context = held.unwrap_or_else(|_| {
                    app.state::<QueueState>()
                        .control
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .unwrap_or(expected)
                });
                let current = finish_stop(&app, &context, error.is_some());
                let same = current.as_ref().is_some_and(|current| {
                    current.revision == context.revision
                        && current.session_id == context.session_id
                        && current.user_id == context.user_id
                });
                if same && error.is_some() {
                    let _ = tauri_plugin_tray::set_meeting_recording_error(&app, current.unwrap());
                } else {
                    let _ = tauri_plugin_tray::set_meeting_recording_context(&app, current);
                }
                let _ = app.emit(
                    CONTROL_EVENT,
                    ControlEvent {
                        action: "excluded",
                        context,
                        error,
                    },
                );
            });
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persisted_hold_blocks_fresh_sessions_until_matching_explicit_resume() {
        let fixture =
            super::super::tests::Fixture::new("user", "http://localhost:8789".into(), false).await;
        let app = fixture.app.handle();
        let mut context = fallback("original", "user");
        context.occurrence.kind = OccurrenceKind::Calendar;
        context.occurrence.key = "individual-calendar-occurrence".into();
        set_context(app, Some(context.clone()), None).unwrap();
        exclude_gate(app, "original", "user").unwrap();
        let held = hold_for_session(app, "original", "user").unwrap();
        let mut duplicate = held.clone();
        duplicate.revision = "second-hold".into();
        set_context(app, Some(duplicate), None).unwrap();
        let latest = hold_for_session(app, "original", "user").unwrap();
        // Simulate a restarted process: the exclusion must still be imported from disk.
        app.state::<QueueState>().holds.lock().unwrap().clear();
        let holds = active_holds(app, "user").unwrap();
        assert_eq!(holds.len(), 2);
        assert_eq!(holds[0].revision, latest.revision);
        assert!(active_holds(app, "other-account").unwrap().is_empty());
        let mut unrelated = fallback("unrelated-session", "user");
        unrelated.occurrence.kind = OccurrenceKind::Calendar;
        unrelated.occurrence.key = "different-calendar-occurrence".into();
        set_context(app, Some(unrelated), None).unwrap();
        let mut next = context.clone();
        next.session_id = "fresh-session".into();
        next.revision = "new-revision".into();
        assert!(set_context(app, Some(next.clone()), None).is_err());
        assert!(set_context(app, Some(next.clone()), Some("stale-revision".into())).is_err());
        assert!(set_context(app, Some(next.clone()), Some(held.revision.clone())).is_err());
        set_context(app, Some(next.clone()), Some(latest.revision.clone())).unwrap();
        assert!(
            held_contexts(&directory(app).unwrap(), "user")
                .unwrap()
                .is_empty()
        );
        prepare_capture(app, "fresh-session", Some("user")).unwrap();
        let current = finish_stop(app, &latest, false).unwrap();
        assert_eq!(current.session_id, "fresh-session");
        assert_eq!(current.state, MeetingRecordingState::Pending);
        assert!(ensure_allowed(app, "original").is_err());
    }

    #[tokio::test]
    async fn held_meeting_resumes_after_a_same_room_recording_context_stops() {
        for previous_user in ["user", "signed-out-account"] {
            let fixture =
                super::super::tests::Fixture::new("user", "http://localhost:8789".into(), false)
                    .await;
            let app = fixture.app.handle();
            let mut original = fallback("held-session", "user");
            original.occurrence.kind = OccurrenceKind::Calendar;
            original.occurrence.key = "calendar-A".into();
            original.occurrence.room = Some("https://meet.google.com/abc-defg-hij".into());
            set_context(app, Some(original.clone()), None).unwrap();
            exclude_gate(app, &original.session_id, "user").unwrap();
            let held = hold_for_session(app, &original.session_id, "user").unwrap();
            finish_stop(app, &held, false);

            let mut unrelated = fallback("other-session", previous_user);
            unrelated.occurrence.kind = OccurrenceKind::Calendar;
            unrelated.occurrence.key = "calendar-B".into();
            unrelated.occurrence.room = original.occurrence.room.clone();
            set_context(app, Some(unrelated.clone()), None).unwrap();
            prepare_capture(app, &unrelated.session_id, Some(previous_user)).unwrap();
            unrelated.state = MeetingRecordingState::Recording;
            set_context(app, Some(unrelated.clone()), None).unwrap();
            let mut resumed = original.clone();
            resumed.session_id = "fresh-A-session".into();
            resumed.revision = "fresh-A-revision".into();
            assert!(set_context(app, Some(resumed.clone()), Some(held.revision.clone())).is_err());
            unrelated.state = MeetingRecordingState::Pending;
            set_context(app, Some(unrelated.clone()), None).unwrap();
            // Runtime fallback can leave only room identity after normal Stop, without a hold.
            unrelated.occurrence.kind = OccurrenceKind::Room;
            *app.state::<QueueState>().control.lock().unwrap() = Some(unrelated);
            set_context(app, Some(resumed.clone()), Some(held.revision)).unwrap();
            prepare_capture(app, &resumed.session_id, Some("user")).unwrap();
            assert!(active_holds(app, "user").unwrap().is_empty());
            assert!(ensure_allowed(app, &original.session_id).is_err());
        }
    }

    #[tokio::test]
    async fn failed_hold_persistence_still_blocks_capture_and_replays_memory_intent() {
        let fixture =
            super::super::tests::Fixture::new("user", "http://localhost:8789".into(), false).await;
        let app = fixture.app.handle();
        let context = fallback("session", "user");
        set_context(app, Some(context.clone()), None).unwrap();
        let dir = directory(app).unwrap();
        // Force atomic replacement to fail without touching user data or permissions.
        std::fs::create_dir(hold_path(&dir, &context.revision)).unwrap();
        exclude_gate(app, "session", "user").unwrap();
        assert!(hold_for_session(app, "session", "user").is_err());
        assert_eq!(active_holds(app, "user").unwrap().len(), 1);
        assert!(prepare_capture(app, "another-session", Some("user")).is_err());
    }
}
