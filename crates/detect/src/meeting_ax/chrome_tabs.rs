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
function collectMeetTabInventory(pid, application) {
    var browser = application(pid);
    if (!browser.running() || browser.id() !== "com.google.Chrome") {
        throw new Error("Target is not running Chrome");
    }
    var records = ["E3_MEET_TABS_V1"];
    var windows = browser.windows();
    for (var i = 0; i < windows.length; i++) {
        var tabs = windows[i].tabs.whose({url: {_beginsWith: "https://meet.google.com/"}})();
        for (var j = 0; j < tabs.length; j++) {
            records.push(String(tabs[j].id()) + "\t" + tabs[j].url());
        }
    }
    if (!browser.running()) {
        throw new Error("Target exited during inventory");
    }
    records.push("E3_MEET_TABS_END");
    return records.join("\n");
}
function run(argv) {
    return collectMeetTabInventory(Number(argv[0]), Application);
}
"#;

#[cfg(target_os = "macos")]
fn query(pid: i32) -> Result<Vec<String>, &'static str> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    let mut child = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e"])
        .arg(SCRIPT)
        .arg(pid.to_string())
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
    if bundle_id != "com.google.Chrome" {
        return None;
    }
    // Retain the application instance: a PID alone can be reused after exit.
    let before = cidre::ns::RunningApp::with_pid(pid)?;
    if before.is_terminated() || before.bundle_id()?.to_string() != bundle_id {
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
    let outcome = query(pid).and_then(|inventory| {
        let after = cidre::ns::RunningApp::with_pid(pid).ok_or("process_missing")?;
        if before.is_terminated() || after.is_terminated() || !before.is_equal(&after) {
            return Err("process_instance_changed");
        }
        if after.bundle_id().is_none_or(|id| id.to_string() != bundle_id) {
            return Err("process_wrong_bundle");
        }
        Ok(inventory)
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

    #[cfg(target_os = "macos")]
    #[test]
    fn script_targets_the_detected_process_and_rejects_invalid_targets() {
        // Exercise the actual production entry point with a fake Application factory.
        // This performs no browser or AppleEvent access.
        let program = format!(
            r#"
function exercise(Application, pid) {{
    {SCRIPT}
    return run([String(pid)]);
}}
function run() {{
    var requested = [];
    function browser(id, url, bundle, stopped) {{
        var checks = 0;
        return {{
            running: function() {{ return !stopped || checks++ === 0; }},
            id: function() {{ return bundle; }},
            windows: function() {{ return [{{ tabs: {{ whose: function(filter) {{
                if (filter.url._beginsWith !== "https://meet.google.com/") {{
                    throw new Error("Unfiltered tab access");
                }}
                return function() {{ return [{{
                    id: function() {{ return id; }},
                    url: function() {{ return url; }}
                }}]; }};
            }} }} }}]; }}
        }};
    }}
    function application(pid) {{
        requested.push(pid);
        if (pid === 11) return browser(1, "https://meet.google.com/aaa-bbbb-ccc", "com.google.Chrome", false);
        if (pid === 42) return browser(2, "https://meet.google.com/abc-defg-hij?authuser=1", "com.google.Chrome", false);
        if (pid === 43) return browser(3, "https://meet.google.com/abc-defg-hij", "com.apple.Safari", false);
        if (pid === 44) return browser(4, "https://meet.google.com/abc-defg-hij", "com.google.Chrome", true);
        if (pid === 46) return {{ running: function() {{ return false; }} }};
        if (pid === 47) return {{
            running: function() {{ return true; }},
            id: function() {{ return "com.google.Chrome"; }},
            windows: function() {{ return []; }}
        }};
        throw new Error("Application can't be found");
    }}
    var result = exercise(application, 42);
    if (requested.length !== 1 || requested[0] !== 42) throw new Error("Wrong process routed");
    [43, 44, 45, 46].forEach(function(pid) {{
        var rejected = false;
        try {{ exercise(application, pid); }} catch (error) {{ rejected = true; }}
        if (!rejected) throw new Error("Invalid target returned an inventory");
    }});
    var empty = exercise(application, 47);
    if (empty !== "E3_MEET_TABS_V1\nE3_MEET_TABS_END") throw new Error("Incomplete empty inventory");
    return result + "\n" + empty;
}}
"#
        );
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", &program])
            .output()
            .expect("macOS provides osascript");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = std::str::from_utf8(&output.stdout).unwrap();
        let (populated, empty) = text.split_once("\nE3_MEET_TABS_V1\n").unwrap();
        assert_eq!(
            parse_inventory(populated),
            Some(vec!["https://meet.google.com/abc-defg-hij:tab:2".into()])
        );
        assert_eq!(parse_inventory(&format!("{HEADER}\n{empty}")), Some(vec![]));
    }

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
