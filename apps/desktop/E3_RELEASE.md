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
4. A transcription provider is optional: with audio retention enabled, detected meetings can record and save raw audio without one. Configure an existing provider only if you also want transcription; this milestone does not create provider credentials.
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
| Hide a quiet muted Chrome Meet, then move its tab to another window | Recording continues as one session while its stable tab ID remains present. |
| Close the captured Chrome Meet tab or navigate it away while Chrome remains running | With Automation granted, recording stops after 15 seconds of confirmed tab absence. |
| Two tabs use the same Meet code; another tab shows an ended/prejoin page | That unrelated page must not end the captured hidden call. |
| Deny Chrome Automation or make its inventory unavailable | No false confirmed departure; retain the conservative fallback. |
| Leave the call, or choose Meet's “End the call for everyone” | Recording stops after the implementation's confirmation period; file remains playable. |
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

After an update, verify Microphone, System audio, and Accessibility inside the app. Switching from an ad-hoc build to the stable signed pilot can require one permission repair. An error while checking a permission is not evidence that permission was denied; retry the check before changing system settings. Repeatedly removing entries or toggling permissions is not an iteration strategy: first verify the installed build's signing identity. Keep the pilot under **System Audio Recording Only**; full screen recording is unnecessary for this milestone.

## Build and install the isolated pilot

Use **E3 Meeting Capture Pilot** (`.github/workflows/e3_capture_pilot.yaml`) in `e3-solutions/anarlog` for a reviewed branch. Merging code does not publish an app. The upstream desktop release workflow depends on upstream CrabNebula infrastructure and is not the E3 pilot release path.

The pilot is Apple Silicon/macOS 15+, with identifier `ai.e3group.meeting-capture-pilot`, its own database and callback scheme, disabled telemetry, a localhost API URL, and no automatic updater. Local Slack/Meet capture needs no backend or transcription provider. Google connector callbacks and scheduled API verification require the separately configured E3 environment; the stock backend does not accept this pilot's scheme. Do not run multiple recorders during testing.

All workflow-packaged pilot builds use Developer ID signing. Keep the same developer team, bundle identifier and Applications path across updates. Developer ID supplies persistent signing identity; notarization separately addresses Gatekeeper's downloaded-app warning. Neither replaces the first-run capture permissions. Verify permissions in the app after a signed update without removing their System Settings entries; signing checks alone do not prove permission persistence. See [Apple's signing requirement explanation](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).

### Remote build modes

- Normal dispatch: validates the certificate and Apple authentication before compilation, then signs, notarizes, staples and verifies Gatekeeper acceptance before uploading `e3-meeting-capture-pilot-arm64-<commit>`.
- `defer_notarization=true`: produces `e3-pilot-signed-NOT-NOTARIZED-<commit>` for local testing and local notarization with an existing Keychain profile. It does not produce an installation-ready distribution artifact.
- `native_checks_only=true`: runs formatting, permission, detection, binding export, UI and storage checks without signing secrets, packaging or artifact uploads.

Full packaging runs only on manual dispatch. During edits, use frontend tests, typechecks and PR checks; package one reviewed candidate when installed-app verification is needed. Native formatting runs before cache initialization to prevent formatting failures from caching an incomplete native target. Native tests and packaging share a Cargo target/cache, but GitHub's capacity, immutable entries and branch rules can still prevent reuse. Pilot release optimization is level 1; production profiles are unchanged.

Configure these repository Actions secrets through secure settings, never source files or chat:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64-encoded Developer ID Application `.p12`, including its private key. |
| `APPLE_CERTIFICATE_PASSWORD` | Password protecting that exported `.p12`. |
| `KEYCHAIN_PASSWORD` | Temporary CI signing-keychain password. |
| `APPLE_ID` | Apple account authorized to notarize for the team. |
| `APPLE_PASSWORD` | Apple app-specific password, not the Mac login password. |
| `APPLE_TEAM_ID` | Certificate's Apple Developer team. |

Deferred mode requires only the certificate, certificate password, keychain password and team. Normal mode additionally makes a read-only, 60-second-bounded authentication check before compiling. A failed preflight cannot guarantee Apple availability later, but prevents spending a build on already rejected credentials. No updater credentials or CrabNebula publication are needed.

### Preserve and resume notarization

The workflow uploads the signed checkpoint before submitting to Apple. Preserve its `BUILD.txt`, `SHA256SUMS` and inner ZIP. If CI submits it, also preserve `e3-pilot-notary-submission-<commit>`: the receipt contains the submission ID, exact ZIP SHA256 and commit. Resume that ID with `notarytool info`/`wait`; do not resubmit or rebuild merely because Apple is delayed. Workflow reruns refuse another submission after an unknown outcome.

For deferred checkpoints, submit the checksum-verified inner ZIP once with an existing `notarytool` Keychain profile and save the returned ID with the archive hash before waiting. Never put a password in commands, source or chat. After Apple returns **Accepted**, staple and validate the app with `xcrun stapler`, verify its signature and `spctl --assess --type execute`, then recreate the ZIP so it contains the ticket. Until those checks pass, label it signed and not notarized.

Pending notarization permits local testing of that exact checkpoint: verify the checksum, Developer ID team and bundle identifier, close the idle previous app, and replace the canonical Applications bundle. Keep quarantine intact and complete normal macOS authentication or app-specific launch confirmations if requested. Do not re-sign the checkpoint, disable Gatekeeper or reset capture permissions to speed iteration.

For a completed normal build, download `e3-meeting-capture-pilot-arm64-<commit>`, check `BUILD.txt` and `SHA256SUMS`, unzip its inner app ZIP and move **E3 Meeting Capture Pilot.app** to Applications with the previous app closed. Complete onboarding, grant Microphone, System Audio Recording and Accessibility, enable **Record detected meetings** and **Stop when meeting ends**, and retain audio (**Forever** for acceptance tests). The pilot does not use the normal automatic-update feed.

### Development and later distribution

Local development requires Node 22+, pnpm 11.1.1, Rust 1.94, and full Xcode with its Metal Toolchain; Command Line Tools alone do not satisfy native MLX dependencies. Run `pnpm install --frozen-lockfile`, then `pnpm dev:desktop` from the root. Use remote builds when local toolchains or disk space are unavailable. Frontend checks do not verify a native app.

A later general E3 distribution can use `node apps/desktop/scripts/build-e3.mjs`, which requires an E3 HTTPS updater feed (`E3_UPDATER_ENDPOINT` with `{{target}}`, `{{arch}}`, `{{current_version}}`), `E3_UPDATER_PUBLIC_KEY`, the corresponding `TAURI_SIGNING_PRIVATE_KEY` and password if encrypted, and an E3-owned `E3_APP_IDENTIFIER`. It validates configuration; it does not provision Apple signing, the feed, OAuth callbacks or backend. Coordinate upstream-derived schemes/package metadata before shipping, and verify a signed update that defers installation during recording. That release feed is outside this isolated pilot.

### Detection limits to verify before team rollout

Automatic detection currently enables Meet and Slack only; other app microphone activity still does not prove an actual supported call. The shared inspection/controller can be extended with platform evidence later. Calls must have readable local Accessibility evidence to start. Allow about 2 seconds plus inspection time for detection. Confirmed local departure uses a 15-second grace period. On macOS, an established Meet keeps recording while its browser process remains running and call evidence is unreadable, including a hidden muted tab; on macOS, disappearance of that exact browser process confirms departure. Slack and Meet on other operating systems retain the fallback: unreadable evidence combined with microphone release uses a conservative 60-second grace, recent far-end audio prevents stopping, and an offline interruption preserves an eight-minute reconnect hold. Notification/DND settings do not govern these checks. A brief evidence gap must not restart a manually stopped call.

Google Chrome on macOS additionally requests Automation permission to read Meet tab IDs and URLs through Chrome’s scripting interface. It reads only Meet tab metadata; Accessibility still provides the positive joined-call evidence required to start. A complete current inventory missing the captured tab confirms closure or navigation away, even if Chrome remains running; the usual 15-second stop grace applies. Moving the same tab between windows preserves its identity. The query targets the detected Chrome process directly and verifies that the same native application instance survived the read. Denial, timeout, malformed inventory, or a terminated/replaced process mean unknown rather than an empty inventory, so a quiet hidden call keeps recording. Another Chrome instance cannot redirect the query; competing active calls remain ambiguous to the controller. Other browsers retain the Accessibility/process fallback and may require manual Stop after an unreadable tab closes. Quiet audio and a muted microphone never establish departure.

Inventory failures back off for 30 seconds, and stopping still requires fresh, attributable departure evidence after the grace. This is not an absolute upper bound: the earlier installed `8c26b0269a` pilot observed one ordinary leave stop after 69.85 seconds and an isolated follow-up after 19.34 seconds. Its bundle-routed process check rejected 5 of 68 attempted inventory reads during five Meet captures; that is not a production failure-rate estimate. The new process-targeted adapter still requires its own installed-app acceptance. While evidence stays unreadable, a running Meet can keep recording indefinitely; use manual Stop when immediate stopping is required. A 20-second observation alone cannot classify a closure failure when backoff is active.

First-use Automation consent, tab movement, closure/navigation, duplicate Meet URLs, manual Stop across permission changes, and permission persistence across signed updates must be verified in the installed signed candidate. Fallback window identities cannot fully disambiguate same-code tab replacements until a unique stable tab identity is established.

Meet continuity uses stable Chrome tab identity when a complete inventory uniquely attributes it; otherwise it falls back to the canonical meeting code and native window identity. An unrelated home page or another meeting's departure page cannot end that recording. Duplicate tabs for the same Meet code within one window remain indistinguishable; use one joined tab per meeting when testing this pilot.

Test switching browser tabs, minimizing the meeting window, muted listening, headset changes, and back-to-back calls carefully. If two calls switch within the same browser process without any observed departure, this version cannot reliably infer a new meeting boundary and may retain one session. Do not claim one session per actual call for that unverified case. Record the observed behavior before wider rollout. Original audio retention permits later session splitting, which is outside this milestone.
