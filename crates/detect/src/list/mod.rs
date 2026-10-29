#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn list_installed_apps() -> Vec<InstalledApp> {
    Vec::new()
}

const SELF_BUNDLE_IDS: &[&str] = &[
    "com.anarlog.dev",
    "com.anarlog.stable",
    "com.anarlog.staging",
    "com.anarlog.nightly",
    "com.hyprnote.dev",
    "com.hyprnote.stable",
    "com.hyprnote.staging",
    "com.hyprnote.nightly",
];

const SELF_APP_NAMES: &[&str] = &[
    "anarlog",
    "anarlog staging",
    "anarlog nightly",
    "hyprnote",
    "hyprnote staging",
    "hyprnote nightly",
    "char",
    "char staging",
    "char nightly",
];

const SELF_APP_PATH_SEGMENTS: &[&str] = &[
    "/anarlog.app/",
    "/anarlog staging.app/",
    "/anarlog nightly.app/",
    "/hyprnote.app/",
    "/hyprnote staging.app/",
    "/hyprnote nightly.app/",
    "/char.app/",
    "/char staging.app/",
    "/char nightly.app/",
];

fn is_self_app(app: &InstalledApp) -> bool {
    let id = app.id.to_lowercase();
    let name = app.name.to_lowercase();

    SELF_BUNDLE_IDS.contains(&id.as_str())
        || SELF_APP_NAMES.contains(&name.as_str())
        || SELF_APP_PATH_SEGMENTS
            .iter()
            .any(|segment| id.contains(segment))
}

pub fn list_mic_using_apps() -> Result<Vec<InstalledApp>, crate::Error> {
    let apps = {
        #[cfg(target_os = "macos")]
        {
            macos::list_mic_using_apps()?
        }
        #[cfg(target_os = "linux")]
        {
            linux::list_mic_using_apps()?
        }
        #[cfg(target_os = "windows")]
        {
            windows::list_mic_using_apps()?
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Vec::<InstalledApp>::new()
        }
    };

    Ok(apps.into_iter().filter(|app| !is_self_app(app)).collect())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct MeetingAudioProcess {
    pub pid: i32,
    pub app: InstalledApp,
    pub input_active: bool,
    pub output_active: bool,
}

pub fn list_meeting_audio_processes() -> Result<Vec<MeetingAudioProcess>, crate::Error> {
    #[cfg(target_os = "macos")]
    {
        Ok(macos::list_meeting_audio_processes()?
            .into_iter()
            .filter(|process| !is_self_app(&process.app))
            .collect())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(crate::Error::AudioProcessQuery(
            "meeting audio process snapshots are unsupported on this platform".to_string(),
        ))
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str) -> InstalledApp {
        InstalledApp {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn is_self_app_matches_only_anarlog_identities() {
        for (id, name, expected) in [
            ("com.anarlog.stable", "Anarlog", true),
            ("com.hyprnote.stable", "Anarlog", true),
            ("com.hyprnote.Hyprnote", "Hyprnote", true),
            ("pid:42", "Anarlog", true),
            ("pid:43", "Char Nightly", true),
            ("pid:44", "Hyprnote Staging", true),
            (
                "/Applications/Anarlog.app/Contents/MacOS/anarlog",
                "Unknown",
                true,
            ),
            (
                "/Applications/Hyprnote Nightly.app/Contents/MacOS/Hyprnote Nightly",
                "Unknown",
                true,
            ),
            ("com.adobe.character-animator", "Character Animator", false),
            (
                "/Applications/Chart.app/Contents/MacOS/Chart",
                "Chart",
                false,
            ),
        ] {
            assert_eq!(
                is_self_app(&app(id, name)),
                expected,
                "is_self_app({id:?}, {name:?})"
            );
        }
    }
}
