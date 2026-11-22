#!/usr/bin/env bash
# Exercise the supported adjacent-version upgrade window against a scratch
# PostgreSQL instance, plus the static epoch-4 deployment prerequisites.
#
# Usage:
#   deploy/tests/rolling-upgrade-matrix.sh --static
#   OLD_REF=<git-ref> NEW_REF=<git-ref> deploy/tests/rolling-upgrade-matrix.sh
#   OLD_IMAGE=<image> NEW_IMAGE=<image> deploy/tests/rolling-upgrade-matrix.sh
#
# --static runs only the filesystem/config assertions and needs neither Docker
# nor a running stack. The full matrix is a manual entry point, not a CI job. It
# requires Docker with network access to the pinned postgres image and enough
# disk for two Access images. It never touches an active installation: the
# database is a throwaway container on loopback port 55490.
#
# It verifies:
#   0. static prerequisites: payload volume/config, broker/profile alignment,
#      relay epoch 4, relay topics, storage readiness and no legacy ack topic;
#   1. the rendered standalone transport/storage settings validate and invalid
#      profiles are rejected before any listener opens;
#   2. an old binary migrates an empty database;
#   3. a new binary migrates the same database forward;
#   4. the old binary refuses the newer schema instead of starting blindly;
#   5. the new binary remains usable.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PORT="${ROLLING_UPGRADE_PORT:-55490}"
PG_NAME="whalerein-rolling-upgrade-$$"
OLD_REF="${OLD_REF:-HEAD~1}"
NEW_REF="${NEW_REF:-HEAD}"
OLD_IMAGE="${OLD_IMAGE:-}"
NEW_IMAGE="${NEW_IMAGE:-}"
ACCESS_CONFIG="$ROOT_DIR/deploy/compose/access.yaml"
STACK_CONFIG="$ROOT_DIR/deploy/compose/stack.yaml"
TEMP_DIRS=()

STATIC_ONLY=0
case "${1:-}" in
  --static) STATIC_ONLY=1 ;;
  "") ;;
  *)
    echo "unknown argument: $1 (supported: --static)" >&2
    exit 2
    ;;
esac

fail() {
  echo "$*" >&2
  exit 1
}

yaml_block() {
  local file="$1" section="$2"
  awk -v section="$section" '
    $0 == section ":" { inside = 1; next }
    inside && /^[^[:space:]]/ { exit }
    inside { print }
  ' "$file"
}

require_static() {
  local description="$1" pattern="$2" text="$3"
  if ! grep -qE "$pattern" <<<"$text"; then
    fail "static prerequisite missing: $description"
  fi
}

require_static_file() {
  local description="$1" pattern="$2" file="$3"
  if ! grep -qE "$pattern" "$file"; then
    fail "static prerequisite missing: $description ($file)"
  fi
}

deny_static_file() {
  local description="$1" pattern="$2" file="$3"
  if grep -rqE --include='*.go' --exclude='*_test.go' "$pattern" "$file"; then
    fail "retired prerequisite still present: $description ($file)"
  fi
}

echo "==> static preflight: relay epoch 4"
require_static_file "broker relay epoch constant" \
  '^const RelayEpoch uint32 = 4$' "$ROOT_DIR/access/internal/broker/budget.go"
require_static_file "proto relay epoch value" \
  'RELAY_EPOCH_V4 = 4;' "$ROOT_DIR/proto/v1/whalerein.proto"
require_static_file "wire contract requires relay_version=4" \
  'relay_version=4' "$ROOT_DIR/proto/v1/WIRE.md"
if grep -qE 'RELAY_EPOCH_V3 = 3;' "$ROOT_DIR/proto/v1/whalerein.proto"; then
  fail "retired relay epoch 3 is still accepted"
fi

echo "==> static preflight: complete-message topics and retired ack topic"
require_static_file "device notice topic" 'func DeviceNoticeTopic' "$ROOT_DIR/access/internal/broker/topics.go"
require_static_file "device ready topic" 'func DeviceReadyTopic' "$ROOT_DIR/access/internal/broker/topics.go"
require_static_file "host live topic" 'func LiveTopic' "$ROOT_DIR/access/internal/broker/topics.go"
require_static_file "device live topic" 'func DeviceLiveTopic' "$ROOT_DIR/access/internal/broker/topics.go"
require_static_file "readiness rejects foreign epoch" \
  'ErrRelayEpoch' "$ROOT_DIR/access/internal/broker/budget.go"
for topic in request result event "/notice" "/ready" "/live"; do
  require_static_file "wire topic $topic" "$topic" "$ROOT_DIR/proto/v1/WIRE.md"
done
deny_static_file "ACK topic in the broker topic parser" '"ack"' "$ROOT_DIR/access/internal/broker/topics.go"
deny_static_file "legacy WRMQTT2 framing" 'WRMQTT2' "$ROOT_DIR/access/internal/broker"

echo "==> static preflight: payload volume and storage config"
storage="$(yaml_block "$ACCESS_CONFIG" storage)"
[ -n "$storage" ] || fail "storage block is missing from deploy/compose/access.yaml"
require_static "storage enabled" '^[[:space:]]*enabled:[[:space:]]*true$' "$storage"
require_static "storage root_dir" '^[[:space:]]*root_dir:[[:space:]]*"/var/lib/whalerein/payloads"$' "$storage"
require_static "storage node_id" '^[[:space:]]*node_id:[[:space:]]*"access-1"$' "$storage"
require_static "storage owner_id" '^[[:space:]]*owner_id:[[:space:]]*"access-1"$' "$storage"
require_static "storage max_object_bytes" '^[[:space:]]*max_object_bytes:[[:space:]]*16777216$' "$storage"
require_static "storage max_host_bytes" '^[[:space:]]*max_host_bytes:[[:space:]]*268435456$' "$storage"
require_static "storage max_node_bytes" '^[[:space:]]*max_node_bytes:[[:space:]]*1073741824$' "$storage"
require_static "storage max_objects" '^[[:space:]]*max_objects:[[:space:]]*1024$' "$storage"
require_static "storage max_active_uploads" '^[[:space:]]*max_active_uploads:[[:space:]]*16$' "$storage"
require_static "storage free-space reserve" '^[[:space:]]*free_space_reserve_bytes:[[:space:]]*1073741824$' "$storage"
require_static "storage retention" '^[[:space:]]*retention:[[:space:]]*24h$' "$storage"
require_static "storage cleanup interval" '^[[:space:]]*cleanup_interval:[[:space:]]*30s$' "$storage"
require_static "storage cleanup batch" '^[[:space:]]*cleanup_batch:[[:space:]]*100$' "$storage"
require_static "storage upload timeout" '^[[:space:]]*upload_timeout:[[:space:]]*2m$' "$storage"
require_static "storage read timeout" '^[[:space:]]*read_timeout:[[:space:]]*60s$' "$storage"
deny_static_file "retired fragment setting" 'fragment_' "$ACCESS_CONFIG"

object_bytes="$(awk '$1 == "max_object_bytes:" {print $2}' <<<"$storage")"
host_bytes="$(awk '$1 == "max_host_bytes:" {print $2}' <<<"$storage")"
node_bytes="$(awk '$1 == "max_node_bytes:" {print $2}' <<<"$storage")"
max_objects="$(awk '$1 == "max_objects:" {print $2}' <<<"$storage")"
active_uploads="$(awk '$1 == "max_active_uploads:" {print $2}' <<<"$storage")"
[ "$host_bytes" -ge "$object_bytes" ] || fail "storage.max_host_bytes is below max_object_bytes"
[ "$node_bytes" -ge "$host_bytes" ] || fail "storage.max_node_bytes is below max_host_bytes"
[ "$active_uploads" -le "$max_objects" ] || fail "storage.max_active_uploads exceeds max_objects"

require_static_file "Compose payload volume mount" \
  'access-payloads:/var/lib/whalerein/payloads' "$STACK_CONFIG"
require_static_file "Compose named payload volume" \
  'name: whalerein-access-payloads' "$STACK_CONFIG"
require_static_file "Access image owns the payload directory" \
  'mkdir -p /var/lib/whalerein/payloads' "$ROOT_DIR/deploy/docker/access.Dockerfile"
require_static_file "database readiness check wired into /readyz" \
  'readiness.Add\("database", diagnostics.DatabaseReadiness\(db\)\)' "$ROOT_DIR/access/cmd/server/main.go"
require_static_file "broker and storage readiness checks wired into /readyz" \
  'serverready.AddChecks\(readiness, service, presence, storageService\)' "$ROOT_DIR/access/cmd/server/main.go"
require_static_file "Compose healthcheck uses /readyz" \
  'http://127.0.0.1:8080/readyz' "$STACK_CONFIG"

echo "==> static preflight: broker ceiling and transport profiles"
require_static_file "broker packet ceiling" \
  '^max_packet_size 1048576$' "$ROOT_DIR/deploy/mosquitto/mosquitto.conf"
require_static_file "entrypoint default broker ceiling" \
  'WHALEREIN_MQTT_MAX_PACKET_SIZE:-1048576' "$ROOT_DIR/deploy/docker/mosquitto-entrypoint.sh"
require_static_file "compose default broker ceiling" \
  'WHALEREIN_MQTT_MAX_PACKET_SIZE:-1048576' "$STACK_CONFIG"
python3 - "$ROOT_DIR/proto/v1/transport_profiles.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    document = json.load(handle)
limit = document["brokerPacketLimitBytes"]
if limit != 1048576:
    raise SystemExit(f"brokerPacketLimitBytes = {limit}, want 1048576")
default = document["profiles"]["default"]
if default["packetBytes"] != 65536 or default["envelopeBytes"] != 16777216:
    raise SystemExit(f"default profile geometry changed: {default}")
for name, profile in document["profiles"].items():
    if profile["packetBytes"] > limit:
        raise SystemExit(f"profile {name} packetBytes exceeds brokerPacketLimitBytes")
    if profile["envelopeBytes"] < profile["packetBytes"]:
        raise SystemExit(f"profile {name} envelopeBytes is below packetBytes")
    if profile["reservedBytes"] < profile["envelopeBytes"]:
        raise SystemExit(f"profile {name} reservedBytes is below envelopeBytes")
PY
transport="$(yaml_block "$ACCESS_CONFIG" transport)"
require_static "transport profile" '^[[:space:]]*profile:[[:space:]]*"\$\{WHALEREIN_TRANSPORT_PROFILE:-default\}"$' "$transport"
require_static "transport receive_maximum" '^[[:space:]]*receive_maximum:[[:space:]]*16$' "$transport"
require_static "transport send_in_flight" '^[[:space:]]*send_in_flight:[[:space:]]*16$' "$transport"
require_static "transport send_queue_depth" '^[[:space:]]*send_queue_depth:[[:space:]]*1024$' "$transport"
require_static "transport send_queue_bytes" '^[[:space:]]*send_queue_bytes:[[:space:]]*33554432$' "$transport"

if [ "$STATIC_ONLY" -eq 1 ]; then
  echo "static deployment preflight passed (no Docker required)"
  exit 0
fi

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "required tool not found: $1" >&2
    exit 3
  }
}

require docker
require git

cleanup() {
  docker rm -f "$PG_NAME" >/dev/null 2>&1 || true
  for dir in "${TEMP_DIRS[@]}"; do
    rm -rf "$dir"
  done
}
trap cleanup EXIT

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
NEW_CONFIG="$ACCESS_CONFIG"
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

echo "==> standalone preflight: rendered transport and storage limits"
rendered="$(check_config "$NEW_IMAGE" "$NEW_CONFIG")"
for expected in \
  "profile=default" \
  "packet_bytes=65536" \
  "envelope_bytes=16777216" \
  "bulk_concurrency=4" \
  "reserved_bytes=33554432" \
  "receive_maximum=16" \
  "send_in_flight=16" \
  "send_queue_depth=1024" \
  "send_queue_bytes=33554432" \
  "result_limit_bytes=16777216" \
  "http_max_body_bytes=25165824"; do
  if ! grep -q "$expected" <<<"$rendered"; then
    echo "rendered transport limit missing: $expected" >&2
    echo "$rendered" >&2
    exit 1
  fi
done
for expected in \
  "enabled=true" \
  "root=/var/lib/whalerein/payloads" \
  "node=access-1" \
  "max_object_bytes=16777216" \
  "max_host_bytes=268435456" \
  "max_node_bytes=1073741824" \
  "max_objects=1024" \
  "max_active_uploads=16" \
  "free_space_reserve_bytes=1073741824" \
  "retention=24h0m0s" \
  "cleanup_interval=30s"; do
  if ! grep -q "$expected" <<<"$rendered"; then
    echo "rendered storage limit missing: $expected" >&2
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

echo "==> standalone preflight: invalid profiles must fail before readiness"
invalid_dir="$(mktemp -d)"
TEMP_DIRS+=("$invalid_dir")
sed 's|^  profile: .*$|  profile: not-a-profile|' \
  "$ACCESS_CONFIG" > "$invalid_dir/unknown-profile.yaml"
if check_config "$NEW_IMAGE" "$invalid_dir/unknown-profile.yaml" >/dev/null 2>&1; then
  echo "unknown transport profile was accepted" >&2
  exit 1
fi
sed 's|^  profile: .*$|  profile: default\n  packet_bytes: 9999999|' \
  "$ACCESS_CONFIG" > "$invalid_dir/invalid-relation.yaml"
if check_config "$NEW_IMAGE" "$invalid_dir/invalid-relation.yaml" >/dev/null 2>&1; then
  echo "a packet budget above brokerPacketLimitBytes was accepted" >&2
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
  echo "old image started against a newer schema; the schema downgrade gate is broken" >&2
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
