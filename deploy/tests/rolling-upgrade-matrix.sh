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
  local image="$1"
  docker run --rm --network host \
    -e WHALEREIN_DATABASE_URL="postgres://postgres@127.0.0.1:${PORT}/whalerein_dev?sslmode=disable" \
    -e WHALEREIN_ADMIN_TOKEN=rolling-upgrade-admin \
    -e WHALEREIN_HOST_PROVISIONING_TOKEN=rolling-upgrade-host \
    -e WHALEREIN_BROKER_PASSWORD=rolling-upgrade-broker \
    -e WHALEREIN_INTERNAL_TOKEN=rolling-upgrade-internal \
    -v "$ROOT_DIR/deploy/compose/access.yaml:/etc/whalerein/access.yaml:ro" \
    "$image" --migrate-only --config /etc/whalerein/access.yaml
}

echo "==> migrate with old image: $OLD_IMAGE"
migrate "$OLD_IMAGE"
echo "==> migrate forward with new image: $NEW_IMAGE"
migrate "$NEW_IMAGE"

echo "==> old image must reject the newer schema"
set +e
output="$(migrate "$OLD_IMAGE" 2>&1)"
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
