#!/usr/bin/env bash
set -euo pipefail

image="${1:?Pass the locally built recordings image}"
name="anlg-recordings-smoke-$$"
volume="${name}-data"
cleanup() {
  docker rm --force "$name" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT
if docker run --rm "$image"; then
  echo "API accepted missing runtime credentials" >&2
  exit 1
fi
if docker run --rm \
  -e SUPABASE_URL=https://example.invalid \
  -e SUPABASE_ANON_KEY=disposable-smoke-key \
  -e SUPABASE_SERVICE_ROLE_KEY=disposable-smoke-key \
  -e NANGO_API_KEY=disposable-smoke-key \
  -e NANGO_WEBHOOK_SIGNING_KEY=disposable-smoke-key "$image"; then
  echo "API accepted missing persistent mount" >&2
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

wait_ready() {
  for ((attempt = 0; attempt < 240; attempt++)); do
    if [[ "$(docker inspect --format '{{.State.Running}}' "$name")" != true ]]; then
      echo "API exited before readiness" >&2
      docker logs "$name"
      exit 1
    fi
    if docker exec "$name" python3 /opt/recordings/entrypoint.py healthcheck; then
      return
    fi
    sleep 1
  done
  echo "API did not become ready" >&2
  return 1
}
wait_ready

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
            if path.startswith("/v1/shared-recordings") or path.startswith("/v1/orgs"):
                assert response.headers["Cache-Control"] == "no-store"
                assert response.headers["X-Content-Type-Options"] == "nosniff"
    except urllib.error.HTTPError as error:
        actual = error.code
        if path.startswith("/v1/shared-recordings") or path.startswith("/v1/orgs"):
            assert error.headers["Cache-Control"] == "no-store"
            assert error.headers["X-Content-Type-Options"] == "nosniff"
    assert actual == expected, (path, actual, expected)
request = urllib.request.Request(base + "/v1/orgs/org/shared-recordings/admit", data=b"", method="POST", headers={"Content-Length": str(140 * 1024 * 1024 + 1)})
try:
    urllib.request.urlopen(request, timeout=3)
except urllib.error.HTTPError as error:
    assert error.code == 413, error.code
else:
    raise SystemExit("Public body limit missing")
print("public health/auth/body-limit/private-ASR smoke passed")
PY

docker exec -i "$name" python3 - <<'PY'
import pathlib, sqlite3
root = pathlib.Path("/data/recordings")
assert root.stat().st_mode & 0o777 == 0o700
with sqlite3.connect(root / "recordings.sqlite") as database:
    assert database.execute("PRAGMA integrity_check").fetchone() == ("ok",)
    assert {"occurrences", "captures", "revisions", "admissions"} <= {row[0] for row in database.execute("SELECT name FROM sqlite_master WHERE type='table'")}
(root / "smoke-persistence-marker").write_text("disposable-volume-proof")
PY
docker restart --time 20 "$name" >/dev/null
wait_ready
docker exec -i "$name" python3 - <<'PY'
import pathlib, sqlite3
root = pathlib.Path("/data/recordings")
assert (root / "smoke-persistence-marker").read_text() == "disposable-volume-proof"
with sqlite3.connect(root / "recordings.sqlite") as database:
    assert database.execute("PRAGMA integrity_check").fetchone() == ("ok",)
listeners = []
for table in ("/proc/net/tcp", "/proc/net/tcp6"):
    for line in pathlib.Path(table).read_text().splitlines()[1:]:
        fields = line.split()
        address, port = fields[1].split(":")
        if int(port, 16) == 18796 and fields[3] == "0A":
            listeners.append(address)
assert listeners == ["0100007F"], listeners
print("persistent restart and private-ASR socket smoke passed")
PY

docker exec -i "$name" python3 - <<'PY'
import io, json, urllib.request, wave
audio = io.BytesIO()
with wave.open(audio, "wb") as wav:
    wav.setnchannels(1)
    wav.setsampwidth(2)
    wav.setframerate(16000)
    wav.writeframes(bytes(32000))
boundary = "isolated-recordings-silence-smoke"
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
echo "API remained running after ASR failure" >&2
exit 1
