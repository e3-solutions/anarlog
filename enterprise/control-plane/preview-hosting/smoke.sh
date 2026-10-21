#!/usr/bin/env bash
set -euo pipefail

image="${1:?Pass the locally built preview image}"
name="anlg-shared-preview-smoke-$$"
volume="${name}-data"
cleanup() {
  docker rm --force "$name" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT
if docker run --rm "$image"; then
  echo "Preview accepted missing runtime credentials" >&2
  exit 1
fi
if docker run --rm \
  -e SUPABASE_URL=https://example.invalid \
  -e SUPABASE_ANON_KEY=disposable-smoke-key \
  -e SUPABASE_SERVICE_ROLE_KEY=disposable-smoke-key \
  -e NANGO_API_KEY=disposable-smoke-key \
  -e NANGO_WEBHOOK_SIGNING_KEY=disposable-smoke-key "$image"; then
  echo "Preview accepted missing persistent mount" >&2
  exit 1
fi
docker volume create "$volume" >/dev/null
docker run --detach --name "$name" \
  --mount "type=volume,source=$volume,target=/data" \
  -p 127.0.0.1::8080 \
  -e SUPABASE_URL=https://example.invalid \
  -e SUPABASE_ANON_KEY=disposable-smoke-key \
  -e SUPABASE_SERVICE_ROLE_KEY=disposable-smoke-key \
  -e NANGO_API_KEY=disposable-smoke-key \
  -e NANGO_WEBHOOK_SIGNING_KEY=disposable-smoke-key \
  "$image" >/dev/null

for ((attempt = 0; attempt < 240; attempt++)); do
  if [[ "$(docker inspect --format '{{.State.Running}}' "$name")" != true ]]; then
    echo "Preview exited before readiness" >&2
    docker logs "$name"
    exit 1
  fi
  if docker exec "$name" python3 /opt/preview/entrypoint.py healthcheck; then
    break
  fi
  sleep 1
done

binding="$(docker port "$name" 8080/tcp)"
python3 - "$binding" <<'PY'
import sys, urllib.request, urllib.error
base = "http://" + sys.argv[1]
for path, expected in (
    ("/health", 200),
    ("/v1/shared-recordings/me", 401),
    ("/v1/orgs/00000000-0000-0000-0000-000000000000/shared-recordings", 401),
    ("/nango/connections", 401),
    ("/v1/audio/transcriptions", 404),
    ("/inference", 404),
):
    try:
        with urllib.request.urlopen(base + path, timeout=3) as response:
            actual = response.status
    except urllib.error.HTTPError as error:
        actual = error.code
    assert actual == expected, (path, actual, expected)
print("public health/auth/private-ASR smoke passed")
PY

docker exec -i "$name" python3 - <<'PY'
import io, json, urllib.request, wave
audio = io.BytesIO()
with wave.open(audio, "wb") as wav:
    wav.setnchannels(1)
    wav.setsampwidth(2)
    wav.setframerate(16000)
    wav.writeframes(bytes(32000))
boundary = "isolated-preview-silence-smoke"
body = (
    f"--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\ntiny.en\r\n"
    f"--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n"
    f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"silence.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
).encode() + audio.getvalue() + f"\r\n--{boundary}--\r\n".encode()
request = urllib.request.Request(
    "http://127.0.0.1:18796/v1/audio/transcriptions", data=body,
    headers={"Content-Type": f"multipart/form-data; boundary={boundary}"}, method="POST",
)
with urllib.request.urlopen(request, timeout=30) as response:
    assert response.status == 200
    assert isinstance(json.load(response)["text"], str)
print("local synthetic-silence ASR smoke passed")
PY
# Kill the actual ASR child; the supervisor must terminate ingress and exit.
docker exec "$name" python3 -c '
import os, pathlib, signal
for path in pathlib.Path("/proc").iterdir():
    if path.name.isdigit():
        try:
            if (path / "comm").read_text().strip() == "whisper-server":
                os.kill(int(path.name), signal.SIGKILL)
                break
        except (OSError, ProcessLookupError):
            pass
else:
    raise SystemExit("ASR child missing")
'
for ((attempt = 0; attempt < 20; attempt++)); do
  if [[ "$(docker inspect --format '{{.State.Running}}' "$name")" != true ]]; then
    [[ "$(docker inspect --format '{{.State.ExitCode}}' "$name")" != 0 ]]
    python3 - "$binding" <<'PY'
import sys, urllib.request, urllib.error
try:
    urllib.request.urlopen("http://" + sys.argv[1] + "/health", timeout=3)
except (OSError, urllib.error.URLError):
    pass
else:
    raise SystemExit("Public ingress remained reachable")
PY
    echo "dependency failure shuts down public ingress: passed"
    exit 0
  fi
  sleep 1
done
echo "Preview remained running after ASR failure" >&2
exit 1
