#!/usr/bin/env bash
# Start real DSH with the checkout installed in a dedicated profile.
# Usage: pnpm start [--profile NAME] [--port PORT] [--reinstall] [--dry-run]
# Internal override: --cloud-environment production|staging|local
# Overrides are saved to the same DSH profile, never a runtime overlay.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PROFILE="${WHALEREIN_DSH_PROFILE:-whalerein-debug}"
PORT="${WHALEREIN_DSH_PORT:-3099}"
REINSTALL=0
DRY_RUN=0
PATCH_ARGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --) ;;
    --profile) PROFILE="${2:?--profile needs a name}"; shift ;;
    --port) PORT="${2:?--port needs a number}"; shift ;;
    --cloud-environment) PATCH_ARGS+=(--cloud-environment "${2:?--cloud-environment needs a value}"); shift ;;
    --reinstall) REINSTALL=1 ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help) sed -n '2,5p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
if [[ ! "$PROFILE" =~ ^[a-zA-Z0-9_-][a-zA-Z0-9_.-]*$ ]]; then
  echo 'profile must be a simple profile name' >&2; exit 2
fi
if [[ ! "$PORT" =~ ^[0-9]{1,5}$ ]] || ((10#$PORT < 1 || 10#$PORT > 65535)); then
  echo 'port must be between 1 and 65535' >&2; exit 2
fi
PROFILE_DIR="${DSH_HOME:-$HOME/.dsh}/profiles/$PROFILE"
PATCH_FILE="$PROFILE_DIR/cordis.patch.yml"
STATE_ROOT="${WHALEREIN_STATE_ROOT:-${WHALEREIN_DATA_DIR:-$ROOT/.data}/dsh/$PROFILE}"
# Validate and render before installation or profile mutations.
PREVIEW="$(python3 "$ROOT/host/dsh/bin/profile-patch.py" "$PATCH_FILE" "$STATE_ROOT" ${PATCH_ARGS[@]+"${PATCH_ARGS[@]}"} --print)"
if [[ "$DRY_RUN" == 1 ]]; then
  printf '%s\n' "$PREVIEW"
  echo "# dsh --profile $PROFILE --port $PORT --no-open"
  exit 0
fi
if ! command -v dsh >/dev/null 2>&1; then
  echo 'dsh is not on PATH' >&2; exit 1
fi
if command -v lsof >/dev/null 2>&1 && lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "port $PORT is already in use" >&2; exit 1
fi
if [[ ! -f "$PROFILE_DIR/package.json" ]]; then
  dsh --profile "$PROFILE" --from-default-profile web --dump-config >/dev/null
fi
if [[ "$REINSTALL" == 1 ]] || ! grep -q '"whalerein-dsh"' "$PROFILE_DIR/package.json" \
   || [[ ! -f "$PROFILE_DIR/node_modules/whalerein-dsh/package.json" ]]; then
  dsh plugin --profile "$PROFILE" add "$ROOT/host/dsh"
fi
python3 - "$PROFILE_DIR/package.json" <<'PY'
import json
import sys
from pathlib import Path
path = Path(sys.argv[1])
data = json.loads(path.read_text(encoding='utf-8'))
bundles = data.setdefault('dsh', {}).setdefault('profile', {}).setdefault('bundles', [])
if 'whalerein-dsh' not in bundles:
    bundles.append('whalerein-dsh')
    path.write_text(json.dumps(data, indent=2) + '\n', encoding='utf-8')
PY
python3 "$ROOT/host/dsh/bin/profile-patch.py" "$PATCH_FILE" "$STATE_ROOT" ${PATCH_ARGS[@]+"${PATCH_ARGS[@]}"}
echo "DSH profile: $PROFILE"
echo "Connection configuration: $PATCH_FILE"
echo 'Press Ctrl-C to stop.'
exec dsh --profile "$PROFILE" --port "$PORT" --no-open
