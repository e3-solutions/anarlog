#!/usr/bin/env bash
set -euo pipefail

image="${1:?Pass the isolated web Docker image tag}"
suffix="$$-$RANDOM"
missing="anlg-preview-web-missing-$suffix"
running="anlg-preview-web-running-$suffix"
response="$(mktemp)"
cleanup() {
  docker rm --force "$missing" "$running" >/dev/null 2>&1 || true
  rm -f "$response"
}
trap cleanup EXIT

docker run --detach --name "$missing" "$image" >/dev/null
for attempt in {1..30}; do
  if [[ "$(docker inspect --format '{{.State.Running}}' "$missing")" == false ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Running}}' "$missing")" != false ]] ||
  [[ "$(docker inspect --format '{{.State.ExitCode}}' "$missing")" == 0 ]]; then
  echo "Web preview must refuse to start without isolated runtime credentials." >&2
  exit 1
fi
case "$(docker logs "$missing" 2>&1)" in
  *"Missing isolated web runtime configuration:"*"SUPABASE_SERVICE_ROLE_KEY"*) ;;
  *) echo "Web preview did not report the expected missing runtime configuration." >&2; exit 1 ;;
esac

docker run --detach --name "$running" \
  --publish 127.0.0.1::3000 \
  --env PORT=3000 \
  --env SUPABASE_URL=https://shared-preview-smoke.invalid \
  --env SUPABASE_ANON_KEY=sb_publishable_smoke_only \
  --env SUPABASE_SERVICE_ROLE_KEY=sb_secret_smoke_only \
  "$image" >/dev/null
published="$(docker port "$running" 3000/tcp)"
port="${published##*:}"
url="http://127.0.0.1:$port/auth/?flow=desktop&scheme=e3-shared-recordings-preview&desktop_state=11111111-1111-4111-8111-111111111111"
for attempt in {1..60}; do
  if curl --silent --show-error --fail --max-time 5 "$url" >"$response" 2>/dev/null; then
    case "$(cat "$response")" in
      *"<html"*) echo "Isolated web Docker smoke passed: missing credentials fail closed; desktop auth page serves HTML."; exit 0 ;;
    esac
  fi
  if [[ "$(docker inspect --format '{{.State.Running}}' "$running")" != true ]]; then
    echo "Isolated web container stopped before its auth route became ready." >&2
    exit 1
  fi
  sleep 1
done
echo "Isolated web auth route did not become ready." >&2
exit 1
