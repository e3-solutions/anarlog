# E3 meeting capture pilot

This milestone detects and records Google Meet and Slack Huddles on the local Mac, including ad hoc calls. It does not enable shared transcripts, uploads, or team retrieval. Existing local recording and transcription machinery remains in use.

## Detection policy

`auto_start_detected_meetings` enables the supported-platform detection policy. A trusted, unambiguous active-call Accessibility surface supplies the meeting evidence. Microphone ownership alone is insufficient to distinguish a call from a browser voice message or prejoin screen; microphone muting alone is insufficient to establish that a call ended. Calendar entries supply context, not proof of attendance.

With **Record detected meetings** enabled, local call detection is the sole authority for recording, for both scheduled and ad hoc calls. Calendar automation cannot create or arm a second recording, including after a manual stop. Explicit scheduled Meet auto-join can still open the validated link when enabled and the matching Google connection is available; opening the link does not itself start recording. Local capture does not require a calendar or Meet API integration.

The separate calendar/API-verified recording path is retained when **Record detected meetings** is off. That path verifies the connected account in the active conference and requires a validated active Meet surface on this Mac before capture. Pending calendar capture is abandoned when detected-meeting mode is enabled. Non-Meet calendar links do not start recordings while detected-meeting mode is on; those platforms require a future local adapter. Turning detected-meeting mode off disables local automatic recording for both supported platforms; disable **Start when meeting begins** as well to disable the separate scheduled path.

Review the final detection implementation for confirmation delays, transient-call handling, rejoin behavior, and stop rules. An ambiguous or unavailable Accessibility observation must not be treated as a confirmed departure. Exact consecutive-call separation depends on the available call identity and should be verified in the pilot.

## First-run setup

1. Use macOS 15 or later on Apple Silicon. Install the isolated pilot build described below. Do not substitute an upstream download: it does not contain these fork changes.
2. Launch the app once. Grant Microphone, Screen & System Audio Recording, and Accessibility permissions in macOS System Settings. Restart the app after granting permissions if detection does not become ready.
3. Enable **Record detected meetings** and automatic stopping in General settings. Keep audio recordings enabled. The managed pilot policy below locks these values when installed.
4. Configure the existing recording/transcription provider, or use the existing record-only mode where available. This milestone does not create provider credentials.
5. Optional: to test the separate scheduled Meet API recording path, turn **Record detected meetings** off, keep scheduled recording on, and connect and enable the employee's primary E3 Google calendar, then connect Google Meet using the same Google account. Both connection identities must match the calendar tracking address.

Deploy `e3/managed_settings.json` beside the active `app.db`, with the app closed. Locate that database through the application's data location rather than assuming a bundle identifier's path. The policy enables launch at login, detected meetings, automatic stopping, audio retention, and automatic updates. Its scheduled setting cannot arm capture while detected-meeting mode is on. Launch once after installation so launch-at-login is applied. Automatic updates require an E3 release feed; a local pilot has the updater disabled.

## Optional calendar/API verification mode

The default detected-meeting pilot does not call the Meet presence API. To exercise the separate scheduled recording mode, disable detected meetings and enable scheduled recording. Deploy `/nango/google-meet/presence` before using that mode. Enable Google Meet API and People API in the configured Google Cloud project. The Meet integration needs `meetings.space.readonly`, `userinfo.profile`, and `userinfo.email`; reconnect existing integrations to grant added scopes.

The current scheduled implementation intentionally accepts only primary calendars under `e3group.ai` or `coreedgesolution.com`. Missing/ambiguous connections, account mismatches, provider failures, or missing scopes prevent the scheduled API path from starting. An active conference beginning more than 15 minutes before its calendar event is rejected; overlapping events sharing a Meet code are skipped. Google supplies no participant-record freshness guarantee, so measure observed join delay. Test external organizers as well as E3 organizers.

## Manual acceptance checklist

Run two short calls per platform with a second person, using a headset for at least one call. Confirm the saved audio contains both voices and the app reports recording without requiring a confirmation click.

| Scenario | Expected result |
| --- | --- |
| Calendar event exists; never join | No recording. |
| Meet prejoin screen or Slack without a Huddle | No recording. |
| Join scheduled Meet on this Mac | One local-detection recording starts; calendar automation does not arm a second capture. |
| Start an ad hoc Meet or Huddle | Recording starts without any calendar event. |
| Keep a second Meet home/prejoin window open while joining one call | The single joined call records; inactive provider pages do not make it ambiguous. |
| Slack shows a channel-named Huddle window without “Huddle in …” | Its live Huddle controls start recording, including while muted; chat addressing stays separately validated. |
| Join muted, then unmute | Capture starts from confirmed attendance and remains one recording. |
| Mute during a call | Recording continues. |
| Leave the call | Recording stops after the implementation's confirmation period; file remains playable. |
| End then immediately start another call | Second call records; no reminder cooldown blocks it. |
| Temporary microphone/device or network interruption | No duplicate recording or premature confirmed departure. |
| Manually stop during a call | It stays stopped for that same call, including when its calendar event is still due; a later distinct call can record. |
| Stop Meet A, record and stop Meet B, then reveal A again | Both calls remain manually stopped until each call's departure is confirmed. |
| Remove Accessibility permission | No unverified automatic start; expose/recover the missing permission. |
| Simultaneous calendar and detected-call triggers | One recorder and one usable session. |
| Scheduled account joins by phone only | Neither detected recording nor the separate scheduled mode starts without local Meet evidence. |
| Quit/restart while a supported call is active | Startup reconciliation follows current call evidence without creating duplicates. |
| Sleep during capture | Capture stops/finalizes safely; retained audio can be opened. |

Record start/stop delay, missed calls, duplicated sessions, and audio completeness against the number of supported calls actually attempted. Mocked tests do not establish real Slack/Meet Accessibility compatibility or audio capture quality.

After replacing this ad-hoc signed pilot, verify Microphone, System audio, and Accessibility inside the app. macOS can retain enabled entries for an older binary. If the app still reports a missing grant, remove and re-add the current application in the corresponding System Settings list and use **Quit & Reopen** when prompted. Keep the pilot under **System Audio Recording Only**; full screen recording is unnecessary for this milestone.

## Local development and installable pilot

The repository currently has no published E3 release. Merging code does not create a downloadable app. The ordinary desktop release workflow references upstream CrabNebula and signing infrastructure; do not run it as an E3 release without replacing that configuration.

For a local development build, install Node 22 or later, pnpm 11.1.1, Rust 1.94, and full Xcode with its Metal Toolchain. Command Line Tools alone are insufficient for native MLX dependencies. Run `pnpm install --frozen-lockfile`, then `pnpm dev:desktop` from the repository root. Local-first desktop development does not require production secrets; authenticated scheduled Meet verification still needs the API/integration configuration above.

The dedicated pilot workflow below builds with an isolated identifier, disabled updater, and ad-hoc signing. A signed/notarized pilot is preferable for a wider team rollout.

The isolated pilot disables native analytics at compile time and uses a localhost API URL; it requires no production telemetry key or backend for local capture.

The pilot uses Rust release optimization level 1 to shorten native build iteration while preserving release checks. Production release profiles are unchanged; measure performance separately before a wider rollout.

Native compilation and packaging require the Rust/Xcode toolchains and sufficient disk space. Frontend tests alone do not verify a distributable build.

## Signed E3 distribution

`node apps/desktop/scripts/build-e3.mjs` requires:

- `E3_UPDATER_ENDPOINT`: E3-controlled HTTPS feed containing `{{target}}`, `{{arch}}`, and `{{current_version}}` placeholders.
- `E3_UPDATER_PUBLIC_KEY`: E3's updater public key.
- `TAURI_SIGNING_PRIVATE_KEY`, plus its password when encrypted: corresponding private key held only in release infrastructure.
- `E3_APP_IDENTIFIER`: E3-owned bundle identifier distinct from upstream.

The script validates these values and applies stable then E3 configuration. It does not provision the feed, credentials, Apple signing/notarization, release pipeline, or integration backend. Before general rollout, publish a signed test update and verify the installed E3 app updates from E3's feed and defers installation during recording.

Stable-derived builds still inherit upstream deep-link schemes and package metadata. They are not automatically side-by-side installations; a permanent independent E3 application requires coordinating callback schemes, OAuth redirects, binary naming, and package metadata. Do not silently replace an employee's existing production installation for this pilot.

## Download the isolated pilot

The `E3 Meeting Capture Pilot` workflow (`.github/workflows/e3_capture_pilot.yaml`) builds an Apple Silicon macOS app for pull requests targeting fork main and on manual dispatch. After a successful run, open its GitHub Actions page, download `e3-meeting-capture-pilot-arm64-<commit>`, unzip the artifact and then its inner app zip, and move **E3 Meeting Capture Pilot.app** to Applications. The artifact includes the exact commit, checksum and installation instructions. Requires Apple Silicon and macOS 15 or later. This build is ad-hoc signed, not Apple-notarized; if blocked, use macOS Privacy & Security → Open Anyway for this deliberately downloaded pilot.

The pilot has its own database/settings folder and deep-link scheme and has automatic updates disabled. Existing Anarlog recordings/settings are not reused. Do not run multiple meeting recorders during the test. Complete local onboarding, grant Microphone, System Audio Recording and Accessibility permissions, enable **Record detected meetings** and **Stop when meeting ends**, and set audio retention to **Forever**. A transcription provider is optional for record-only testing. Do not assume the normal automatic-update feed installs this pilot.

The isolated pilot can test local Slack and ad-hoc Meet capture without a calendar connection. Google account login/connector callbacks require the E3 scheme to be allowed in a separately configured web/API environment; the stock backend does not accept this pilot scheme. The artifact does not provision that backend, OAuth registration or the Meet presence endpoint. Verify scheduled API attendance separately in the configured E3 environment; local positive call detection remains available without it.

### Detection limits to verify before team rollout

Automatic detection currently enables Meet and Slack only; other app microphone activity still does not prove an actual supported call. The shared inspection/controller can be extended with platform evidence later. Calls must have readable local Accessibility evidence to start. Allow about 2 seconds plus inspection time for detection. Confirmed local departure uses a 15-second grace period. On macOS, an established Meet keeps recording while its browser process remains running and call evidence is unreadable, including a hidden muted tab; on macOS, disappearance of that exact browser process confirms departure. Slack and Meet on other operating systems retain the fallback: unreadable evidence combined with microphone release uses a conservative 60-second grace, recent far-end audio prevents stopping, and an offline interruption preserves an eight-minute reconnect hold. Notification/DND settings do not govern these checks. A brief evidence gap must not restart a manually stopped call.

Closing a Meet tab while its browser remains running is indistinguishable from hiding an ongoing call with the current Accessibility snapshot. Click **Stop** manually when closing or leaving an unreadable/background Meet; automatic stopping requires positive departure evidence or the browser process exiting on macOS. Quiet audio and a muted microphone alone never establish that a Meet ended.

Meet continuity uses its canonical meeting code and native window identity. An unrelated home page or another meeting's departure page cannot end that recording. Duplicate tabs for the same Meet code within one window remain indistinguishable; use one joined tab per meeting when testing this pilot.

Test switching browser tabs, minimizing the meeting window, muted listening, headset changes, and back-to-back calls carefully. If two calls switch within the same browser process without any observed departure, this version cannot reliably infer a new meeting boundary and may retain one session. Do not claim one session per actual call for that unverified case. Record the observed behavior before wider rollout. Original audio retention permits later session splitting, which is outside this milestone.
