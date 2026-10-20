# Anarlog Enterprise control plane

This commercially licensed service packages the workspace-scoped enterprise capture API with automatic PostgreSQL migrations, fail-closed startup configuration, and graceful shutdown. It durably appends provider-neutral capture events, projects each revision into the shared session-ingest contract, and delivers those revisions to authorized clients.

## Evaluation deployment

Docker with Compose v2 is required. From the repository root:

```sh
cp enterprise/control-plane/.env.sample enterprise/control-plane/.env
```

Replace both instances of the database password with the same random hexadecimal value, and replace the workspace bearer token with at least 32 random characters. Hexadecimal values avoid URL-encoding differences between `POSTGRES_PASSWORD` and `ANARLOG_ENTERPRISE_DATABASE_URL`.

Start the service:

```sh
docker compose \
  --file enterprise/control-plane/compose.yaml \
  --env-file enterprise/control-plane/.env \
  up --build --wait --detach
```

The bundle binds the API to `127.0.0.1:8080` by default and does not publish PostgreSQL. Verify it with:

```sh
curl --fail http://127.0.0.1:8080/health/ready
curl --fail \
  --header 'Authorization: Bearer YOUR_WORKSPACE_TOKEN' \
  'http://127.0.0.1:8080/v1/workspaces/evaluation-workspace/session-envelopes?consumerId=evaluation-device&after=0'
```

Stop it without deleting database data:

```sh
docker compose \
  --file enterprise/control-plane/compose.yaml \
  --env-file enterprise/control-plane/.env \
  down
```

## Infisical

The deployment accepts every secret through environment variables, so Infisical can inject them without generating a `.env` file. Store the variables from `.env.sample` in one Infisical environment, then run:

```sh
infisical run --env=prod -- \
  docker compose \
    --file enterprise/control-plane/compose.yaml \
    up --build --wait --detach
```

See the official [`infisical run` documentation](https://infisical.com/docs/cli/commands/run) for machine identity and project options. Do not use `--watch` for a production Compose process; perform a controlled restart when credentials rotate.

## Configuration

| Variable                                      | Required | Purpose                                                                                                    |
| --------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------- |
| `ANARLOG_ENTERPRISE_DATABASE_URL`             | Yes      | PostgreSQL connection URL. Startup fails if the database is unavailable or migrations fail.                |
| `ANARLOG_ENTERPRISE_WORKSPACE_TOKENS`         | Yes      | JSON object mapping workspace IDs to bearer tokens. At least one unique token of 32–512 bytes is required. |
| `ANARLOG_ENTERPRISE_BIND_ADDRESS`             | No       | Listener address. Defaults to `0.0.0.0:8080`; the image sets the same value.                               |
| `ANARLOG_ENTERPRISE_DATABASE_MAX_CONNECTIONS` | No       | PostgreSQL pool size from 1–100. Defaults to `10`.                                                         |
| `ANARLOG_ENTERPRISE_ZOOM_CLIENT_ID`           | Together | Zoom RTMS app client ID.                                                                                   |
| `ANARLOG_ENTERPRISE_ZOOM_CLIENT_SECRET`       | Together | Zoom RTMS app client secret.                                                                               |
| `ANARLOG_ENTERPRISE_ZOOM_WEBHOOK_SECRET`      | Together | Zoom webhook secret used to verify signed request bodies.                                                  |
| `ANARLOG_ENTERPRISE_ZOOM_ACCOUNT_WORKSPACES`  | Together | JSON object mapping signed Zoom account IDs to configured workspace IDs.                                   |
| `RUST_LOG`                                    | No       | Standard tracing filter. Defaults to request and service information. No telemetry is exported.            |

`GET /health/live` is process-only liveness. `GET /health/ready` checks PostgreSQL. Capture and delivery routes require `Authorization: Bearer <token>` and reject a token used against any workspace other than its configured workspace.

The four Zoom variables are optional as a group. When all are absent, `/webhooks/zoom` is not mounted and the control plane boots with its minimal database and workspace-auth configuration. If any Zoom variable is set, all four must be valid or startup fails. `ANARLOG_ENTERPRISE_ZOOM_ACCOUNT_WORKSPACES` may reference only workspaces present in `ANARLOG_ENTERPRISE_WORKSPACE_TOKENS`. Infisical should inject these values at process start; the service does not fetch, persist, or log provider credentials.

Capture workers create a durable job with `POST /v1/workspaces/{workspace_id}/capture-jobs/{job_id}`, claim it through `/claim`, and renew the returned 60-second fencing lease through `/lease`. Every new event appended to `/events` must carry that lease identity. An expired lease can be reclaimed with a higher epoch, which prevents the prior worker from advancing the job; an identical already-persisted event remains safe to replay. Event IDs and zero-based sequences are idempotency keys. Each accepted event atomically advances the PostgreSQL checkpoint and publishes a delivery revision, while conflicting IDs, sequences, lifecycle transitions, or stale leases fail closed.

The static token map is intended for the evaluation bundle and sits behind the `WorkspaceAuthenticator` interface. Production OIDC, SCIM, offline license enforcement, object storage, and meeting browser workers are outside this service.

## Local shared-recording milestone

`shared-recordings-local` runs an opt-in, loopback-only recording coordinator using a separate private SQLite/file directory. It uses this service's workspace authentication and the existing `owhisper-client` batch adapters. It neither opens the desktop database nor changes capture, local retention, detection, or private E2EE backups. The normal PostgreSQL deployment does not enable these routes.

Selection with explicit gap fallback was chosen over aligning and mixing every capture: it recovers known missing coverage while avoiding duplicate far-end speech and an unvalidated cross-device synchronization engine. It does require externally verified offsets and cannot automatically improve microphone quality or remove echo already present inside a source. [Meet conference records](https://developers.google.com/workspace/meet/api/reference/rest/v2/conferenceRecords) and [Slack huddle events](https://docs.slack.dev/reference/events/user_huddle_changed/) supply occurrence identities that recurring URLs/channels cannot provide.

The new planner uses existing [hound 3.5.1](https://docs.rs/crate/hound/3.5.1/source/Cargo.toml) (Apache-2.0) for WAV I/O and existing SHA-256/serde dependencies; no codec binary is bundled. Existing rodio/Symphonia and rubato remain the normalization path in the fork. [whisper.cpp v1.8.1](https://github.com/ggml-org/whisper.cpp/tree/v1.8.1) (MIT) was used only as an isolated local inference fixture through the existing client adapter. FFmpeg was considered for media processing; its [build-dependent LGPL/GPL licensing](https://ffmpeg.org/legal.html) and binary packaging are unnecessary for the normalized-WAV coordinator. The hosted Deepgram adapter uses its existing [pre-recorded audio API](https://developers.deepgram.com/reference/speech-to-text/listen-pre-recorded); credentials, provider terms, and operational validation remain external to the local fixture.

Set `ANARLOG_SHARED_RECORDINGS_DIR` to an absolute disposable directory and `ANARLOG_ENTERPRISE_WORKSPACE_TOKENS` to a JSON workspace/service-token map, then run:

```sh
cargo run --manifest-path enterprise/Cargo.toml --locked \
  -p anarlog-enterprise-control-plane --bin shared-recordings-local
```

The listener prints its allocated loopback address. `ANARLOG_SHARED_RECORDINGS_BIND` optionally selects a loopback address/port. Only one process can open a recording directory. Keep this directory separate from desktop storage.

For transcription, configure all three variables `ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_URL`, `ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_ADAPTER` (`deepgram` or `openai`), and `ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_MODEL`. An optional `ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_KEY` authenticates the provider. The OpenAI adapter also works with a local compatible whisper.cpp server. HTTPS is required except for loopback HTTP. Without this configuration, raw ingest, canonicalization, download, and deletion work; `/process` is absent.

All routes below require the workspace service bearer token and begin with `/v1/workspaces/{workspace}/shared-recordings/{id}`:

| Request | Behavior |
| --- | --- |
| `POST /` | Admit `{provider, providerOccurrenceId, durationFrames}`. Providers are `google_meet` or `slack_huddle`. |
| `POST /captures/{sourceId}` | Accept `{metadata, wavBase64}` as one complete immutable source segment. |
| `POST /finalize` | Snapshot the current accepted input set into a canonical revision; identical selection is a no-op. |
| `POST /process` | Finalize, transcribe the exact resulting WAV, and save only if revision and audio checksum still match. Retry safely after provider failure. |
| `POST /seal` | Close admission and finalize. New segments then conflict; identical retries still succeed. |
| `GET /` | Read admission, sources, seal state, pending-input state, and current revision. |
| `GET /audio`, `GET /transcript` | Read current canonical audio or its revision/transcript status. |
| `GET /revisions/{number}`, `GET /revisions/{number}/audio` | Read the historical revision and its exact audio. |
| `DELETE /captures/{sourceId}`, `DELETE /` | Revoke affected derived histories before deleting source/derived files. |

Admission is an assertion by a **trusted workspace operator**, not provider verification performed by this server. Meet requires `conferenceRecords/<occurrence>`, not a recurring Meet URL. Slack requires `<teamId>/<callId>`, not a channel. The operator must independently verify occurrence binding, participation/sharing authority, and timing; calendar membership, AX hashes, and local tab IDs are insufficient. This token grants access to every admitted recording in that workspace. Uploader/device fields are provenance asserted by that operator, not per-user authentication or an ACL. Unresolved local captures are not uploaded or matched automatically.

Source metadata includes `uploaderId`, `deviceId`, persisted `localCaptureId`, zero-based `segmentSequence`, `localSessionId`, `localTranscriptId`, `sha256`, `offsetFrames`, `rank`, `gaps`, and required `timingBasis` (`operator_verified` or `provider_timeline`). The immutable retry identity is workspace/uploader/device/capture/segment sequence. Reusing it or a source ID with different metadata/bytes returns `409`; the first accepted input remains authoritative. Do not retry a conflict indefinitely or give the same bytes a new identity to bypass it. A genuinely new recording uses a new capture ID.

Audio must be decoded 16 kHz mono signed PCM16 WAV. Normalize formats using the existing audio tooling before upload; the coordinator deliberately adds no codec or alignment engine. Each source segment is bounded to 100 MiB (about 54.6 minutes); use separately identified segments for longer captures. There are at most 32 source segments per occurrence, a four-hour output timeline, and an eight-hour combined decoded-source budget. Missing segments, early stops, and late starts leave real timeline gaps; output never squeezes these intervals together. Uploads become eligible only after the whole segment, decoded format, and checksum validate. Interrupted uploads can retry the same complete segment; byte-range resume is deferred.

Lower `rank` wins, with source ID as the deterministic tie-breaker. Select exactly one source per interval; fill only declared raw gaps or unavailable coverage. Silence/mute remains valid audio. `gaps` are half-open source-frame ranges, independent of transcription failures. `offsetFrames` maps the source's first decoded frame to the fixed occurrence timeline and must be calibrated independently of device wall clocks. Transitions hard-cut at the exact verified frame boundary, preferring the selected primary outside its gaps. There is no crossfade, duplicate summation, acoustic alignment, clock-drift correction, or automatic quality scoring. Different start/stop times and negative offsets are clipped against the fixed timeline. Uncovered ranges become silence and remain explicit in the selection manifest.

Before sealing, an explicit `/finalize` or `/process` is a checkpoint, not a declaration that every participant has uploaded. A selected late source creates a new immutable revision with transcription pending; an unused source preserves the current revision/transcript. The operator seals after collecting the desired inputs; no calendar timer infers that a call ended. Each transcript records provider/model under its exact manifest revision and WAV SHA-256. Changing the configured provider/model conflicts with an existing transcript instead of silently relabeling it. A provider failure leaves the audio retryable. A crash after provider success but before saving can repeat paid provider work; exactly-once external billing is not promised.

Original segments and historical outputs are retained until explicit deletion; there is no automatic retention TTL. Deleting any source revokes all derived histories immediately, then removes their files. `/finalize` can recompute from remaining sources, including after sealing. Tombstones prevent deleted identities from being uploaded again; provenance/identity metadata remains in the local registry. Startup removes interrupted writes, unreferenced files, and pending physical deletion. Exports or copies downloaded by another client are outside this local deletion boundary.

This milestone proves the central ingest/selection/revision/transcription flow locally. Automatic desktop enqueue, verified provider resolvers, real multi-device calibration, user/role authorization, hosted storage adapters, shared-session delivery, production retention, and production deployment remain separate integrations. No deployment or installed-pilot change is required to test it.

The runtime image uses pinned Debian and Rust base-image digests, runs as UID/GID `10001`, has a read-only root filesystem in Compose, and persists only PostgreSQL data. Terminate TLS at a trusted reverse proxy before exposing the API beyond localhost.
