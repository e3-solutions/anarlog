# E3 meeting capture pilot

This milestone detects and records Google Meet and Slack Huddles on the local Mac, including ad hoc calls. It does not enable shared transcripts, uploads, or team retrieval. Existing local recording and transcription machinery remains in use.

## Detection policy

`auto_start_detected_meetings` enables the supported-platform detection policy. A trusted, unambiguous active-call Accessibility surface supplies the meeting evidence. Microphone ownership alone is insufficient to distinguish a call from a browser voice message or prejoin screen; microphone muting alone is insufficient to establish that a call ended. Calendar entries supply context, not proof of attendance.

With **Record detected meetings** enabled, local call detection is the sole authority for recording, for both scheduled and ad hoc calls. Calendar automation cannot create or arm a second recording, including after a manual stop. Explicit scheduled Meet auto-join can still open the validated link when enabled and the matching Google connection is available; opening the link does not itself start recording. Local capture does not require a calendar or Meet API integration.

The separate calendar/API-verified recording path is retained when **Record detected meetings** is off. That path verifies the connected account in the active conference and requires a validated active Meet surface on this Mac before capture. Pending calendar capture is abandoned when detected-meeting mode is enabled. Non-Meet calendar links do not start recordings while detected-meeting mode is on; those platforms require a future local adapter. Turning detected-meeting mode off disables local automatic recording for both supported platforms; disable **Start when meeting begins** as well to disable the separate scheduled path.

Review the final detection implementation for confirmation delays, transient-call handling, rejoin behavior, and stop rules. An ambiguous or unavailable Accessibility observation must not be treated as a confirmed departure. Exact consecutive-call separation depends on the available call identity and should be verified in the pilot.

## Per-meeting recording opt-out

Signed-in E3 Scribe users can open **E3 Recording** in the Mac menu bar and choose **Stop for this meeting**. The menu changes to **E3 Off** after capture stops and the choice is saved. The note header and calendar event popover provide the same control. No confirmation dialog is required.

The choice applies to this account's recorder on this device and survives app restart and rejoining the meeting. A uniquely identified calendar occurrence is held individually. When the calendar occurrence cannot be identified, the room stays off until explicit Resume, including when the room is reused. An unidentified call keeps recording off conservatively for the account on this device until Resume. These broader holds are explained under the in-app information control.

**Resume recording** starts a fresh session. It never reopens excluded audio or clears the exclusion on an older session. Already accepted remote audio or processing cannot be retracted, and this control does not stop another person's recorder or delete earlier notes.

If stopping or saving cannot be confirmed, the menu shows **E3 Check recording** and offers **Retry stop for this meeting**. Keep recording off until the error is resolved. Native fixture tests and the synthetic [interaction demo](e3/recording-opt-out-demo.gif) are separate evidence; the demo does not establish real-call recording or installed-app behavior.

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

## Local development and installable pilot

The repository currently has no published E3 release. Merging code does not create a downloadable app. The ordinary desktop release workflow references upstream CrabNebula and signing infrastructure; do not run it as an E3 release without replacing that configuration.

For a local development build, install Node 22 or later, pnpm 11.1.1, Rust 1.94, and full Xcode with its Metal Toolchain. Command Line Tools alone are insufficient for native MLX dependencies. Run `pnpm install --frozen-lockfile`, then `pnpm dev:desktop` from the repository root. Local-first desktop development does not require production secrets; authenticated scheduled Meet verification still needs the API/integration configuration above.

The dedicated pilot workflow below keeps an isolated identifier and disabled updater. Its normal installable build uses Developer ID signing and Apple notarization; ad-hoc signing is an explicit diagnostic option.

The isolated pilot disables native analytics at compile time and uses a localhost API URL; it requires no production telemetry key or backend for local capture.

The pilot uses Rust release optimization level 1 to shorten native build iteration while preserving release checks. Production release profiles are unchanged; measure performance separately before a wider rollout.

Native compilation and packaging require the Rust/Xcode toolchains and sufficient disk space. Frontend tests alone do not verify a distributable build.

### Fast iteration and stable permissions

Ad-hoc signatures identify a particular binary rather than a persistent developer. macOS can therefore treat a rebuild as a different application for privacy approval. Keep `ai.e3group.meeting-capture-pilot`, the signing team, and the installation path stable across updates. A Developer ID signature supplies identity continuity; notarization separately addresses the downloaded-app Gatekeeper warning. Changing to that identity still needs initial approval, and macOS can independently require later confirmation. See [Apple's signing requirement explanation](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).

Use normal frontend tests, typechecks, and PR checks during edits. Dispatch **E3 Meeting Capture Pilot** explicitly when a native test or installable candidate is needed. Full app packaging no longer starts on every PR push. Native tests and packaging share the same cached Cargo target directory, so the test compilation is retained as well as the application compilation. Before dependency installation or native compilation, certificate import is validated explicitly as PKCS#12 and a read-only `notarytool history` request verifies Apple notarization access with a 60-second deadline. A failed authentication check stops the run without uploading an app, exposing raw credential diagnostics, or spending time compiling. This preflight cannot guarantee that Apple's service remains available later.

Native formatting runs before the native cache is initialized, so a formatting-only failure cannot save an incomplete cache under the compiled-dependency key. GitHub cache capacity and immutable entries still limit reuse; a successful cache restore does not prove compiled dependencies were restored.

For native permission and meeting-detection regression checks without signing or app packaging, dispatch **E3 Meeting Capture Pilot** in native-check mode: `gh workflow run e3_capture_pilot.yaml -R e3-solutions/anarlog --ref <reviewed-branch> -f native_checks_only=true`. It runs the permission, browser activation, Chrome tab inventory, browser inspection, Slack and storage tests on macOS, plus binding export and the shared UI build, without Apple credentials, signing, packaging or artifact uploads. It uses the same job and native cache as packaging within GitHub's branch/cache access rules. Run this check for a native change, then package a single reviewed candidate when needed; passing these tests does not establish installed-app behavior.

A completed Developer ID signed app is uploaded separately as `e3-pilot-signed-NOT-NOTARIZED-<commit>` before Apple submission. That diagnostic checkpoint is not the final installation artifact. The normal pilot artifact remains gated on Apple acceptance, stapling and Gatekeeper verification. If Apple is delayed, preserve the checkpoint and the `e3-pilot-notary-submission-<commit>` receipt (submission ID and exact ZIP hash), and resume that ID with `notarytool`; do not resubmit blindly or rebuild just to recover the app. Workflow reruns deliberately refuse a second notarization submission. Native dependency caches are saved on failed runs as well, so a later notarization failure does not itself discard successful compilation; GitHub cache scope, capacity and cancellation can still prevent reuse. Select `developer-id` for an app intended to be installed; missing signing credentials fail before dependency installation or native compilation. The `ad-hoc` option remains unnotarized and can repeat permission prompts after replacement. Permission persistence still requires an actual update between two distinct signed builds on the test Mac; signing checks alone do not prove it.

Pending notarization does not block local testing of that exact signed checkpoint. Verify its checksum, Developer ID team and bundle identifier, close the idle previous app, and update the canonical Applications bundle with normal macOS launch and quarantine intact. Verify the existing capture permissions in the app rather than resetting them. If macOS requires authentication or an app-specific confirmation, complete that normal system handoff; do not disable Gatekeeper or remove quarantine. To build a signed checkpoint while CI notarization credentials are unavailable, explicitly select `defer_notarization=true`; final distribution remains gated on Apple acceptance.

Configure these GitHub Actions secrets for the fork using secure repository settings, never source files or chat:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64-encoded E3 Developer ID Application `.p12`, including its private key. |
| `APPLE_CERTIFICATE_PASSWORD` | Password protecting that exported `.p12`. |
| `KEYCHAIN_PASSWORD` | Password used for the temporary CI signing keychain. |
| `APPLE_ID` | Apple account authorized to notarize for the signing team. |
| `APPLE_PASSWORD` | An Apple app-specific password for notarization, not the Mac login password. |
| `APPLE_TEAM_ID` | The Apple Developer team issuing the certificate. |

Use the existing Apple certificate import and desktop signing infrastructure. The pilot workflow overrides its diagnostic `signingIdentity: "-"` configuration for the signed build and requires signature, stapled-ticket, and Gatekeeper validation before uploading that artifact. Keep the same signing identity available for subsequent candidates. Updater credentials and CrabNebula publication are unnecessary for this Actions-artifact pilot.

Check the downloaded `BUILD.txt` before replacing the app. Do not substitute an ad-hoc artifact for a signed candidate during an iteration test. After the first signed installation and permission grant, install a second distinct build and verify that detection and both audio channels work without removing permission entries; automated signing checks alone do not establish permission persistence on this Mac.

To sign an already compiled pilot without rebuilding Rust, use `scripts/sign-e3-pilot-local.py` on macOS with `--output-dir`, the full Developer ID Application `--identity`, and optionally an unlocked signing `--keychain`. It copies the input app into a fresh output directory, preserves each executable's entitlements, signs the bundled code before the app, and verifies the result. Only FinderInfo and ResourceFork packaging metadata are removed from that copy; quarantine remains intact. The installed/source app is not modified.

Add `--notary-profile <profile>` to submit with an existing `notarytool` Keychain profile, staple Apple's accepted ticket, validate Gatekeeper acceptance, and rebuild the final ZIP. Without that option the output explicitly reports `notarized: false`; signing alone does not clear downloaded-app warnings. Save notarization credentials interactively with `xcrun notarytool store-credentials <profile> --apple-id <account> --team-id <team>` and enter the app-specific password only at its hidden prompt. The script accepts no passwords.

## Signed E3 distribution

`node apps/desktop/scripts/build-e3.mjs` requires:

- `E3_UPDATER_ENDPOINT`: E3-controlled HTTPS feed containing `{{target}}`, `{{arch}}`, and `{{current_version}}` placeholders.
- `E3_UPDATER_PUBLIC_KEY`: E3's updater public key.
- `TAURI_SIGNING_PRIVATE_KEY`, plus its password when encrypted: corresponding private key held only in release infrastructure.
- `E3_APP_IDENTIFIER`: E3-owned bundle identifier distinct from upstream.

The script validates these values and applies stable then E3 configuration. It does not provision the feed, credentials, Apple signing/notarization, release pipeline, or integration backend. Before general rollout, publish a signed test update and verify the installed E3 app updates from E3's feed and defers installation during recording.

Stable-derived builds still inherit upstream deep-link schemes and package metadata. They are not automatically side-by-side installations; a permanent independent E3 application requires coordinating callback schemes, OAuth redirects, binary naming, and package metadata. Do not silently replace an employee's existing production installation for this pilot.

## Download the isolated pilot

Dispatch the `E3 Meeting Capture Pilot` workflow (`.github/workflows/e3_capture_pilot.yaml`) for the reviewed branch and select `developer-id`. After a successful run, open its GitHub Actions page, download `e3-meeting-capture-pilot-arm64-<commit>`, unzip the artifact and then its inner app zip, and move **E3 Meeting Capture Pilot.app** to Applications with the previous app closed. The artifact includes the exact commit, signing mode, checksum and installation instructions. Requires Apple Silicon and macOS 15 or later. The default signed build must pass notarization checks; an explicitly selected ad-hoc diagnostic build still requires an app-specific macOS exception and is unsuitable for testing permission persistence across updates.

The pilot has its own database/settings folder and deep-link scheme and has automatic updates disabled. Existing Anarlog recordings/settings are not reused. Do not run multiple meeting recorders during the test. Complete local onboarding, grant Microphone, System Audio Recording and Accessibility permissions, enable **Record detected meetings** and **Stop when meeting ends**, and set audio retention to **Forever**. A transcription provider is optional for record-only testing. Do not assume the normal automatic-update feed installs this pilot.

The isolated pilot can test local Slack and ad-hoc Meet capture without a calendar connection. Google account login/connector callbacks require the E3 scheme to be allowed in a separately configured web/API environment; the stock backend does not accept this pilot scheme. The artifact does not provision that backend, OAuth registration or the Meet presence endpoint. Verify scheduled API attendance separately in the configured E3 environment; local positive call detection remains available without it.

### Detection limits to verify before team rollout

Automatic detection currently enables Meet and Slack only; other app microphone activity still does not prove an actual supported call. The shared inspection/controller can be extended with platform evidence later. Calls must have readable local Accessibility evidence to start. Allow about 2 seconds plus inspection time for detection. Confirmed local departure uses a 15-second grace period. On macOS, an established Meet keeps recording while its browser process remains running and call evidence is unreadable, including a hidden muted tab; on macOS, disappearance of that exact browser process confirms departure. Slack and Meet on other operating systems retain the fallback: unreadable evidence combined with microphone release uses a conservative 60-second grace, recent far-end audio prevents stopping, and an offline interruption preserves an eight-minute reconnect hold. Notification/DND settings do not govern these checks. A brief evidence gap must not restart a manually stopped call.

Google Chrome on macOS additionally requests Automation permission to read Meet tab IDs and URLs through Chrome’s scripting interface. It reads only Meet tab metadata; Accessibility still provides the positive joined-call evidence required to start. A complete current inventory missing the captured tab confirms closure or navigation away, even if Chrome remains running; the usual 15-second stop grace applies. Moving the same tab between windows preserves its identity. Denial, timeout, malformed inventory, or multiple Chrome processes mean unknown rather than an empty inventory, so a quiet hidden call keeps recording. Other browsers retain the Accessibility/process fallback and may require manual Stop after an unreadable tab closes. Quiet audio and a muted microphone never establish departure.

Inventory failures back off for 30 seconds. When the next query succeeds, confirmed absence still needs the 15-second stop grace and polling time. Observe a closure for at least 50 seconds before classifying it as a failure; persistent unavailable evidence can require manual Stop. A 20-second observation is insufficient when that backoff is active.

First-use Automation consent, tab movement, closure/navigation, duplicate Meet URLs, manual Stop across permission changes, and permission persistence across signed updates must be verified in the installed signed candidate. Legacy window identities cannot fully disambiguate same-code tab replacements until a unique stable tab identity is established.

Meet continuity uses its canonical meeting code and native window identity. An unrelated home page or another meeting's departure page cannot end that recording. Duplicate tabs for the same Meet code within one window remain indistinguishable; use one joined tab per meeting when testing this pilot.

Test switching browser tabs, minimizing the meeting window, muted listening, headset changes, and back-to-back calls carefully. If two calls switch within the same browser process without any observed departure, this version cannot reliably infer a new meeting boundary and may retain one session. Do not claim one session per actual call for that unverified case. Record the observed behavior before wider rollout. Original audio retention permits later session splitting, which is outside this milestone.
