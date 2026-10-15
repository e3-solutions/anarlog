# E3 desktop release and remote updates

The direct-distribution app already checks for updates every 30 minutes, downloads them automatically, and installs at launch when no meeting is active. The setting defaults to on. An E3 fork must use its own signed release feed: the ordinary stable build points at Anarlog's feed and trusts Anarlog's signing key.

For an E3 build, set these values in the release environment:

- `E3_UPDATER_ENDPOINT`: E3-controlled HTTPS feed URL with `{{target}}`, `{{arch}}`, and `{{current_version}}` placeholders. The service must return a Tauri-compatible signed update manifest and artifact for each supported platform.
- `E3_UPDATER_PUBLIC_KEY`: public half of E3's Tauri updater signing key pair.
- `TAURI_SIGNING_PRIVATE_KEY`: corresponding private key, held only in the release environment. Set `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if the key is encrypted.
- `E3_APP_IDENTIFIER`: an E3-owned bundle identifier distinct from `com.hyprnote.*`.

Then build from the repository root with `node apps/desktop/scripts/build-e3.mjs` and optional Tauri build flags such as `--target`. The script applies the stable configuration, the macOS stable override when applicable, and finally an E3 override. It fails if any required value is missing or if the update feed/key/bundle identifier reuse the upstream values. Do not distribute a binary built through the ordinary stable command as an E3 release.

Before rollout, provision the feed and publish a signed test release through the E3 release pipeline. Verify an installed E3 build updates from that feed on each supported platform, that a meeting defers installation, and that the release is signed/notarized where required by the OS. The build script does not create the feed, signing key, release pipeline, or OS code-signing credentials.

The current E3 build keeps Anarlog's desktop deep-link schemes and Linux package replacement metadata. Treat it as the organization's Anarlog installation and remove an upstream Anarlog install before rollout. A side-by-side install needs separate deep-link, login callback, binary, and Linux package work; changing the bundle identifier alone does not provide that.

## Managed meeting capture

Deploy [`e3/managed_settings.json`](e3/managed_settings.json) beside each person's active `app.db` before the first app launch. The policy keeps launch at login, scheduled meeting auto-start, active Slack huddle auto-start, and automatic updates enabled. Anarlog applies launch-at-login on startup; the installer must start the app once after installation. Users must connect and enable only their E3 work calendars, grant microphone and system-audio permissions, and choose a transcription provider. Scheduled auto-start considers every enabled calendar event with a meeting link and an accepted or organizer RSVP; the managed policy cannot distinguish E3 events from personal calendars. Slack huddle auto-start requires trusted Accessibility access and a matching active Slack microphone session.

## Shared calendar transcripts

Create an E3 shared workspace and provision each recorder as a member. Add its canonical lowercase UUID as `settings.shared_transcript_workspace_id` to the deployed `managed_settings.json`; the checked-in policy omits this value because a workspace ID has not been provisioned. A malformed ID prevents app database startup. Each user must sign in with an active Pro subscription. When the managed workspace ID is present, the desktop app automatically enables Cloud API & Connectors for that signed-in account. The managed policy does not grant Pro entitlement or workspace membership; provision both before rollout. In managed workspace mode, the Cloud API uploads only completed calendar-backed meetings with a finalized, nonempty transcript and shares those with E3. Existing Cloud API content uploaded before the managed policy is applied is not removed by this change.

With these prerequisites, the desktop client uploads its Cloud API snapshot and shares it after a calendar-backed recording has a completed, nonempty transcript. In-progress captures and unscheduled Slack huddles are not published. If two E3 attendees record the same calendar occurrence, the server keeps the first workspace share and the second client treats the duplicate as complete. The client retries transient publication failures with its pending snapshot queue. The API and database migration for workspace sharing must be deployed before enabling the managed workspace ID.
