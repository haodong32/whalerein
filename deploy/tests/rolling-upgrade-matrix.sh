#!/usr/bin/env bash
# Exercise the supported adjacent-version upgrade window against a scratch
# PostgreSQL instance.
#
# Usage:
#   OLD_REF=<git-ref> NEW_REF=<git-ref> deploy/tests/rolling-upgrade-matrix.sh
#   OLD_IMAGE=<image> NEW_IMAGE=<image> deploy/tests/rolling-upgrade-matrix.sh
#
# The script is a manual matrix entry point, not a CI job. It requires Docker
# with network access to the pinned postgres image and enough disk for two
# Access images. It never touches an active installation: the database is a
# throwaway container on loopback port 55490.
#
# It verifies:
#   0. the rendered standalone transport settings validate and invalid profiles
#      are rejected before any listener opens;
#   1. an old binary migrates an empty database;
#   2. a new binary migrates the same database forward;
#   3. the old binary refuses the newer schema instead of starting blindly;
#   4. the new binary remains usable.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PORT="${ROLLING_UPGRADE_PORT:-55490}"
PG_NAME="whalerein-rolling-upgrade-$$"
OLD_REF="${OLD_REF:-HEAD~1}"
NEW_REF="${NEW_REF:-HEAD}"
OLD_IMAGE="${OLD_IMAGE:-}"
NEW_IMAGE="${NEW_IMAGE:-}"
TEMP_DIRS=()

cleanup() {
  docker rm -f "$PG_NAME" >/dev/null 2>&1 || true
  for dir in "${TEMP_DIRS[@]}"; do
    rm -rf "$dir"
  done
}
trap cleanup EXIT

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "required tool not found: $1" >&2
    exit 3
  }
}

require docker
require git

build_image() {
  local ref="$1" tag="$2" dir
  dir="$(mktemp -d)"
  TEMP_DIRS+=("$dir")
  git -C "$ROOT_DIR" archive "$ref" | tar -x -C "$dir"
  docker build -f "$dir/deploy/docker/access.Dockerfile" -t "$tag" "$dir" >/dev/null
}

if [ -z "$OLD_IMAGE" ]; then
  build_image "$OLD_REF" "whalerein-rolling-upgrade-old:$$"
  OLD_IMAGE="whalerein-rolling-upgrade-old:$$"
fi
if [ -z "$NEW_IMAGE" ]; then
  build_image "$NEW_REF" "whalerein-rolling-upgrade-new:$$"
  NEW_IMAGE="whalerein-rolling-upgrade-new:$$"
fi

# Each image consumes the configuration that shipped with its own revision.
# The hard cut adds a transport block the older binary must not be asked to
# parse while it is proving the schema path.
config_for() {
  local ref="$1" dir
  # Keep the bind-mount source under the repository tree; Docker Desktop only
  # shares selected host paths, and a directory mount here would hide the file.
  mkdir -p "$ROOT_DIR/.data"
  dir="$(mktemp -d "$ROOT_DIR/.data/rolling-upgrade-config.XXXXXX")"
  TEMP_DIRS+=("$dir")
  git -C "$ROOT_DIR" show "$ref:deploy/compose/access.yaml" > "$dir/access.yaml"
  printf '%s\n' "$dir/access.yaml"
}
NEW_CONFIG="$ROOT_DIR/deploy/compose/access.yaml"
if [ -z "${OLD_CONFIG:-}" ]; then
  OLD_CONFIG="$(config_for "$OLD_REF")"
fi

docker run -d --rm --name "$PG_NAME" \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_DB=whalerein_dev \
  -e POSTGRES_HOST_AUTH_METHOD=trust \
  -p "127.0.0.1:${PORT}:5432" \
  postgres:18-alpine >/dev/null

for _ in $(seq 1 60); do
  if docker exec "$PG_NAME" pg_isready -U postgres -d whalerein_dev >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker exec "$PG_NAME" pg_isready -U postgres -d whalerein_dev >/dev/null

migrate() {
  local image="$1" config="${2:-$NEW_CONFIG}"
  docker run --rm --network host \
    -e WHALEREIN_DATABASE_URL="postgres://postgres@127.0.0.1:${PORT}/whalerein_dev?sslmode=disable" \
    -e WHALEREIN_ADMIN_TOKEN=rolling-upgrade-admin \
    -e WHALEREIN_HOST_PROVISIONING_TOKEN=rolling-upgrade-host \
    -e WHALEREIN_BROKER_PASSWORD=rolling-upgrade-broker \
    -e WHALEREIN_INTERNAL_TOKEN=rolling-upgrade-internal \
    -v "$config:/etc/whalerein/access.yaml:ro" \
    "$image" --migrate-only --config /etc/whalerein/access.yaml
}

check_config() {
  local image="$1" config="$2"
  docker run --rm --network host \
    -e WHALEREIN_DATABASE_URL="postgres://postgres@127.0.0.1:${PORT}/whalerein_dev?sslmode=disable" \
    -e WHALEREIN_ADMIN_TOKEN=rolling-upgrade-admin \
    -e WHALEREIN_HOST_PROVISIONING_TOKEN=rolling-upgrade-host \
    -e WHALEREIN_BROKER_PASSWORD=rolling-upgrade-broker \
    -e WHALEREIN_INTERNAL_TOKEN=rolling-upgrade-internal \
    -e WHALEREIN_TRANSPORT_PROFILE="${WHALEREIN_TRANSPORT_PROFILE:-default}" \
    -v "$config:/etc/whalerein/access.yaml:ro" \
    "$image" --check-config --config /etc/whalerein/access.yaml
}

echo "==> standalone preflight: rendered transport limits"
rendered="$(check_config "$NEW_IMAGE" "$NEW_CONFIG")"
for expected in   "profile=default" \
  "packet_bytes=65536" \
  "fragment_payload_bytes=61440" \
  "fragment_count=274" \
  "envelope_bytes=16777216" \
  "bulk_concurrency=4" \
  "reserved_bytes=33554432" \
  "result_limit_bytes=16777216" \
  "http_max_body_bytes=25165824"; do
  if ! grep -q "$expected" <<<"$rendered"; then
    echo "rendered transport limit missing: $expected" >&2
    echo "$rendered" >&2
    exit 1
  fi
done
roomy="$(WHALEREIN_TRANSPORT_PROFILE=roomy check_config "$NEW_IMAGE" "$NEW_CONFIG")"
for expected in "profile=roomy" "envelope_bytes=33554432" "bulk_concurrency=8" "http_max_body_bytes=47185920"; do
  if ! grep -q "$expected" <<<"$roomy"; then
    echo "rendered alternate transport limit missing: $expected" >&2
    echo "$roomy" >&2
    exit 1
  fi
done
if ! grep -q '^max_packet_size 4194304$' "$ROOT_DIR/deploy/mosquitto/mosquitto.conf"; then
  echo "broker packet ceiling is not rendered at the standalone default" >&2
  exit 1
fi
if ! grep -q 'WHALEREIN_MQTT_MAX_PACKET_SIZE' "$ROOT_DIR/deploy/compose/stack.yaml"; then
  echo "compose does not parameterize the broker packet ceiling" >&2
  exit 1
fi

echo "==> standalone preflight: invalid profiles must fail before readiness"
invalid_dir="$(mktemp -d)"
TEMP_DIRS+=("$invalid_dir")
sed 's|^  profile: .*$|  profile: not-a-profile|' \
  "$ROOT_DIR/deploy/compose/access.yaml" > "$invalid_dir/unknown-profile.yaml"
if check_config "$NEW_IMAGE" "$invalid_dir/unknown-profile.yaml" >/dev/null 2>&1; then
  echo "unknown transport profile was accepted" >&2
  exit 1
fi
awk '{print} /^  profile: / {print "  fragment_payload_bytes: 999999"}' \
  "$ROOT_DIR/deploy/compose/access.yaml" > "$invalid_dir/invalid-relation.yaml"
if check_config "$NEW_IMAGE" "$invalid_dir/invalid-relation.yaml" >/dev/null 2>&1; then
  echo "invalid transport relation was accepted" >&2
  exit 1
fi

echo "==> migrate with old image: $OLD_IMAGE"
migrate "$OLD_IMAGE" "$OLD_CONFIG"
echo "==> migrate forward with new image: $NEW_IMAGE"
migrate "$NEW_IMAGE"

echo "==> old image must reject the newer schema"
set +e
output="$(migrate "$OLD_IMAGE" "$OLD_CONFIG" 2>&1)"
status=$?
set -e
if [ "$status" -eq 0 ]; then
  echo "old image started against a newer schema; the compatibility gate is broken" >&2
  exit 1
fi
if ! grep -q "newer migration" <<<"$output"; then
  echo "old image failed for an unexpected reason:" >&2
  echo "$output" >&2
  exit 1
fi

echo "==> new image remains usable"
migrate "$NEW_IMAGE"

echo "rolling upgrade matrix passed (forward migration only; downgrade is rejected by design)"
