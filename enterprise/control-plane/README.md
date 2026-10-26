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

The runtime image uses pinned Debian and Rust base-image digests, runs as UID/GID `10001`, has a read-only root filesystem in Compose, and persists only PostgreSQL data. Terminate TLS at a trusted reverse proxy before exposing the API beyond localhost.

## Shared meeting recordings

`shared-recordings-app` is the single recording API for the opt-in desktop and hosted flow. It stores shared organization recordings in a separate private SQLite/file directory, uses one OpenAI-compatible JSON transcription endpoint, and leaves desktop databases and private E2EE backups separate. The PostgreSQL capture service does not mount these routes.

The planner reuses [hound 3.5.1](https://docs.rs/crate/hound/3.5.1/source/Cargo.toml) (Apache-2.0) for WAV I/O and existing SHA-256/serde dependencies. Native normalization uses the existing rodio/Symphonia and rubato path; no FFmpeg binary is required. The hosted preview bundles MIT-licensed whisper.cpp v1.8.1, Whisper tiny.en weights, and Silero VAD v5.1.2 through that private JSON endpoint. Pinned sources, model checksums, and redistribution notices are in [preview-hosting/third-party](preview-hosting/third-party/PROVENANCE.txt).

### User-authenticated desktop preview

For a local backend, set `SUPABASE_URL`, `SUPABASE_ANON_KEY`, and an absolute `ANARLOG_SHARED_RECORDINGS_DIR`, then run `cargo run --manifest-path enterprise/Cargo.toml --locked -p anarlog-enterprise-control-plane --bin shared-recordings-app`. The listener binds to loopback port 8789 by default; `ANARLOG_SHARED_RECORDINGS_BIND` can select another address; a non-loopback address additionally requires the exact setting `ANARLOG_SHARED_RECORDINGS_ALLOW_PUBLIC_BIND=true`. Provider connections additionally require `NANGO_API_KEY`, `NANGO_WEBHOOK_SIGNING_KEY`, and `SUPABASE_SERVICE_ROLE_KEY`; `NANGO_API_BASE` is optional. The service uses existing Supabase auth, memberships, and owned Nango connections; it does not provision them. Configure `ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_URL` and `ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_MODEL`, with an optional `ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_KEY`. Without provider connections, admission stays unresolved; without transcription configuration, uploaded audio remains pending. The endpoint receives the canonical WAV as multipart with the configured model and `response_format=json`. It returns text only; the stored batch envelope keeps `words` empty and does not claim word timestamps. Provider redirects and responses over 8 MiB are rejected. The prototype Deepgram adapter is not supported.

Its routes start at `/v1/orgs/{workspace}/shared-recordings`; `/v1/shared-recordings/me` discovers the signed-in user's organizations and owned connections. Each request verifies the app JWT and fresh organization membership through Supabase RLS. Uploads require prior provider-verified admission for that user; the server derives uploader identity and forces device-clock-hint timing. Organization members read the same library and exact-revision audio/transcript. Source deletion requires its uploader or an administrator; occurrence deletion/sealing requires an administrator.

Meet admission reuses the existing verified-presence helper: resolve the active conference record and prove the connected Google account is currently present. Slack admission requires a connected human user token with `users:read`; `auth.test` identifies that user and `users.info` provides their current `profile.huddle_state_call_id`. Bot tokens, inactive Huddles, room aliases without active Meet presence, and unresolved identities fail closed. Each occurrence has a stable server-stored epoch; client clock calibration is a hint for audio alignment, never proof of alignment.

Build the desktop with `VITE_SHARED_RECORDINGS_PREVIEW_URL` pointing to the loopback service or the isolated hosted HTTPS endpoint, with matching test Supabase/API/web configuration. Separate Tauri overlays are `tauri.shared-recordings-preview.json` and `tauri.shared-recordings-preview-second.json`; each has its own bundle ID, storage folder, and auth callback scheme. Use a web sign-in build which allows those callback schemes. Do not install over or reconfigure `E3 Meeting Capture Pilot.app`.

Sharing starts disabled. The UI chooses an organization and exposes its published recordings, source selections, missing coverage, transcript, and outbox retry status. While recording, positive accessibility evidence prompts verified provider admission. Native stop snapshots retain a private original before existing local-retention cleanup, normalize with existing audio utilities into five-minute PCM16 WAV segments, and retry immutable account/org/device/capture uploads using fresh bearer tokens. Storage failures or decoded-length mismatches quarantine the retained copy rather than sharing an uncertain timeline. Declared raw gaps are widened conservatively for bounded codec padding. Retry copies are removed after upload/processing succeeds; playback downloads expire after 24 hours while running or at the next startup. STT outages do not create raw-audio gaps. The 32-source limit means at most 160 aggregate recorded minutes with these segments; longer-call segmentation/capacity tuning is deferred.

### Audio and revision guarantees

Audio must be decoded 16 kHz mono signed PCM16 WAV. Normalize formats using the existing audio tooling before upload; the coordinator reuses the existing audio-sync estimator and adds no codec binary. Each source segment is bounded to 100 MiB (about 54.6 minutes); use separately identified segments for longer captures. There are at most 32 source segments per occurrence, a four-hour output timeline, and an eight-hour combined decoded-source budget. Missing segments, early stops, and late starts leave real timeline gaps; output never squeezes these intervals together. Uploads become eligible only after the whole segment, decoded format, and checksum validate. Interrupted uploads can retry the same complete segment; byte-range resume is deferred.

Lower `rank` wins, with source ID as the deterministic tie-breaker. Select exactly one source per interval; fill only declared raw gaps or unavailable coverage. Silence/mute remains valid audio. `gaps` are half-open source-frame ranges, independent of transcription failures. `offsetFrames` maps the source's first decoded frame to the occurrence timeline. The authenticated app forces rank zero and treats device clocks as hints; audio evidence must verify their alignment. Transitions hard-cut at the exact verified frame boundary, preferring the selected primary outside its gaps. There is no crossfade, duplicate summation, clock-drift resampling, or automatic quality scoring. For device-clock hints, the existing audio-sync GCC-PHAT estimator scans nonoverlapping energetic windows and must confirm at least two independent anchors with strong waveform correlation and consistent lag; otherwise only one capture group is used. Negative offsets are preserved: `manifest.timeline_start_frame` records the earliest negative frame, and WAV coordinates are rebased to retain pre-epoch audio. A later input that extends the beginning can therefore change the origin and revision. Uncovered ranges become silence and remain explicit in the selection manifest.

Processing is a checkpoint, not a declaration that every participant has uploaded. A selected late source creates a new immutable revision with transcription pending; an unused source preserves the current revision/transcript. An organization administrator can seal after collecting the desired inputs; no calendar timer infers that a call ended. Each transcript records provider/model under its exact manifest revision and WAV SHA-256. Changing the configured provider/model conflicts with an existing transcript instead of silently relabeling it. A provider failure leaves the audio retryable. A crash after provider success but before saving can repeat paid provider work; exactly-once external billing is not promised.

Original segments and historical outputs are retained until explicit deletion; there is no automatic retention TTL. Deleting any source revokes all derived histories immediately, then removes their files. Processing can recompute from remaining sources, including after sealing. Tombstones prevent deleted identities from being uploaded again; provenance/identity metadata remains in the local registry. Startup removes interrupted writes, unreferenced files, and pending physical deletion. Exports or copies downloaded by another client are outside this local deletion boundary.

### Hosted shared-recording preview

[preview-hosting/railway.toml](preview-hosting/railway.toml) selects the Dockerfile in the same directory, built from the repository root. The entrypoint runs CPU-only whisper.cpp with tiny.en and Silero VAD on loopback port 18796, then explicitly enables public binding for `shared-recordings-app` on `$PORT`. The existing Axum server exposes `/health`, `/v1/*`, and `/nango/*` directly; Railway terminates public HTTPS. The transcription endpoint stays private. `/health` reports process liveness, while the entrypoint supervises both child services.

Mount one dedicated durable volume at `/data`. The entrypoint stores SQLite and recording objects in `/data/recordings`, checksum-verified models in `/data/models`, and private runtime files in `/data/runtime`, with private directory permissions and UID/GID `10001`. It configures the OpenAI-compatible local transcription adapter automatically and shuts down public ingress if a child service exits.

Required runtime variables are `SUPABASE_URL`, `SUPABASE_ANON_KEY`, `SUPABASE_SERVICE_ROLE_KEY`, `NANGO_API_KEY`, and `NANGO_WEBHOOK_SIGNING_KEY`. Set `ANARLOG_SHARED_RECORDINGS_WEB_ORIGIN` to the exact hosted sign-in HTTPS origin, without a trailing slash or path, to extend the existing local-client CORS allowlist. Supply secrets at runtime; none are baked into the image.

Two separately signed-in native previews on one Mac have been tested against the isolated hosted service: verified Meet identity, complementary opening/closing audio, overlap deduplication, exact source PCM splices, identical organization-wide audio/transcript revisions for both accounts, and retained-upload recovery across a real backend outage. The recordings were reprocessed after the sparse-speech alignment fix, without replacing their source audio. Transcript decision content was verified with explicit wording variants; this is not a word-perfect transcription claim. A Slack capture and shared playback were also tested with one Slack principal.

Testing on two physical Macs and initial background Meet detection are deferred from this change. The completed core-flow test established identity with the active call visible; it does not establish unattended first detection. Production retention and capacity policy remain separate operational work. Downloaded copies remain outside server deletion/revocation boundaries.
