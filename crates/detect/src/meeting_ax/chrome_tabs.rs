//! Optional Chrome lifecycle evidence. Tab inventory never establishes attendance.

const HEADER: &str = "E3_MEET_TABS_V1";
const END: &str = "E3_MEET_TABS_END";

pub(super) fn canonical_meet_url(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("meet.google.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let code = super::platform::google_meet_code_from_title(&format!(
        "Meet - {}",
        url.path().trim_matches('/')
    ))?;
    url.set_path(&format!("/{code}"));
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

pub(super) fn parse_inventory(output: &str) -> Option<Vec<String>> {
    let mut lines = output.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let mut identities = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let mut ended = false;
    for line in lines.by_ref() {
        if line == END {
            ended = true;
            break;
        }
        let (id, value) = line.split_once('\t')?;
        if id.is_empty()
            || !id.bytes().all(|byte| byte.is_ascii_digit())
            || id.parse::<u64>().ok()? == 0
            || !value.starts_with("https://meet.google.com/")
            || value.contains('\t')
            || identities.len() >= 4096
            || !seen_ids.insert(id)
        {
            return None;
        }
        // Meet home/prejoin without a canonical code is not a captured call.
        if let Some(url) = canonical_meet_url(value) {
            identities.push(format!("{url}:tab:{id}"));
        }
    }
    (ended && lines.next().is_none()).then_some(identities)
}

pub(super) fn unique_identity(url: &str, inventory: &[String]) -> Option<String> {
    let prefix = format!("{url}:tab:");
    let mut matching = inventory
        .iter()
        .filter(|identity| identity.starts_with(&prefix));
    let first = matching.next()?;
    matching.next().is_none().then(|| first.clone())
}

#[cfg(target_os = "macos")]
const SCRIPT: &str = r#"
if application id "com.google.Chrome" is not running then return "NOT_RUNNING"
set resultText to "E3_MEET_TABS_V1" & linefeed
with timeout of 1 seconds
    tell application id "com.google.Chrome"
        repeat with browserWindow in every window
            repeat with meetingTab in (every tab of browserWindow whose URL starts with "https://meet.google.com/")
                set resultText to resultText & (id of meetingTab as text) & (character id 9) & (URL of meetingTab as text) & linefeed
            end repeat
        end repeat
    end tell
end timeout
return resultText & "E3_MEET_TABS_END"
"#;

#[cfg(target_os = "macos")]
fn sole_chrome_pid(pid: i32) -> bool {
    let apps = super::running_meeting_apps();
    let mut chrome = apps.iter().filter(|(app, _)| app.id == "com.google.Chrome");
    chrome
        .next()
        .is_some_and(|(_, running_pid)| *running_pid == pid)
        && chrome.next().is_none()
}

#[cfg(target_os = "macos")]
fn query() -> Result<Vec<String>, &'static str> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    let mut child = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(SCRIPT)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "spawn")?;
    let stdout = child.stdout.take().ok_or("stdout")?;
    let reader = thread::spawn(move || -> Result<Vec<u8>, &'static str> {
        let mut bytes = Vec::new();
        stdout
            .take(131_073)
            .read_to_end(&mut bytes)
            .map_err(|_| "read")?;
        (bytes.len() <= 131_072)
            .then_some(bytes)
            .ok_or("output_limit")
    });
    let deadline = Instant::now() + Duration::from_millis(1500);
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success().then_some(()).ok_or("nonzero_exit"),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(15)),
            state => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(if state.is_err() { "wait" } else { "timeout" });
            }
        }
    };
    let bytes = reader.join().map_err(|_| "reader_panic")??;
    outcome?;
    parse_inventory(std::str::from_utf8(&bytes).map_err(|_| "utf8")?).ok_or("parse")
}

#[cfg(target_os = "macos")]
pub(super) fn inspect(bundle_id: &str, pid: i32) -> Option<Vec<String>> {
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };
    static ATTEMPTS: OnceLock<Mutex<HashMap<i32, Instant>>> = OnceLock::new();
    if bundle_id != "com.google.Chrome" || !sole_chrome_pid(pid) {
        return None;
    }
    let attempts = ATTEMPTS.get_or_init(Default::default);
    {
        let mut attempts = attempts.lock().unwrap_or_else(|error| error.into_inner());
        if attempts
            .get(&pid)
            .is_some_and(|previous| previous.elapsed() < Duration::from_secs(30))
        {
            return None;
        }
        // Reserve before IPC. Failures, including permission prompts, back off.
        attempts.insert(pid, Instant::now());
    }
    let started = Instant::now();
    let outcome = query().and_then(|inventory| {
        sole_chrome_pid(pid)
            .then_some(inventory)
            .ok_or("pid_changed")
    });
    tracing::debug!(
        status = outcome.as_ref().err().copied().unwrap_or("complete"),
        count = outcome.as_ref().map_or(0, Vec::len),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "chrome_tab_inventory_query"
    );
    let result = outcome.ok();
    if result.is_some() {
        attempts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&pid);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_success_can_mean_empty_inventory() {
        assert_eq!(
            parse_inventory("E3_MEET_TABS_V1\nE3_MEET_TABS_END\n"),
            Some(vec![])
        );
        for value in [
            "",
            "NOT_RUNNING",
            "E3_MEET_TABS_V1\n",
            "E3_MEET_TABS_V1\nwrong\nE3_MEET_TABS_END",
            "E3_MEET_TABS_V1\nE3_MEET_TABS_END\ntrailing",
        ] {
            assert_eq!(parse_inventory(value), None);
        }
        for record in [
            "not-an-id\thttps://meet.google.com/abc-defg-hij",
            "0\thttps://meet.google.com/abc-defg-hij",
            "42\thttps://meet.google.com/abc-defg-hij\textra",
            "42\thttps://meet.google.com/abc-defg-hij\n42\thttps://meet.google.com/aaa-bbbb-ccc",
        ] {
            assert_eq!(parse_inventory(&format!("{HEADER}\n{record}\n{END}")), None);
        }
    }

    #[test]
    fn tab_identity_survives_window_changes_but_not_navigation_or_duplicate_urls() {
        let inventory = parse_inventory("E3_MEET_TABS_V1\n42\thttps://meet.google.com/abc-defg-hij?authuser=1#x\nE3_MEET_TABS_END").unwrap();
        assert_eq!(
            inventory,
            vec!["https://meet.google.com/abc-defg-hij:tab:42"]
        );
        assert_eq!(
            unique_identity("https://meet.google.com/abc-defg-hij", &inventory),
            Some(inventory[0].clone())
        );
        let mut duplicate = inventory.clone();
        duplicate.push("https://meet.google.com/abc-defg-hij:tab:43".into());
        assert_eq!(
            unique_identity("https://meet.google.com/abc-defg-hij", &duplicate),
            None
        );
        assert_eq!(
            unique_identity("https://meet.google.com/aaa-bbbb-ccc", &inventory),
            None
        );
        assert_eq!(
            parse_inventory("E3_MEET_TABS_V1\n42\thttps://example.com/\nE3_MEET_TABS_END"),
            None
        );
    }

    #[test]
    fn only_canonical_meet_origins_can_bind_to_a_call() {
        for value in [
            "http://meet.google.com/abc-defg-hij",
            "https://meet.google.com.evil.test/abc-defg-hij",
            "https://user@meet.google.com/abc-defg-hij",
            "https://meet.google.com/",
            "https://meet.google.com/abc-defg-hij/extra",
        ] {
            assert_eq!(canonical_meet_url(value), None);
        }
    }
}
