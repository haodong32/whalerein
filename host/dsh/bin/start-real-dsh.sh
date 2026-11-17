#!/usr/bin/env bash
# Start real DSH with the WhaleRein plugin loaded, then stay in the foreground
# until Ctrl-C.
#
#   cd host/dsh
#   pnpm start                 # reuse the profile; fresh profiles are managed
#   pnpm start --managed       # seed/rewrite the entry for WhaleRein Cloud
#   pnpm start --standalone    # seed/rewrite the entry for a manual Access host
#
# The dedicated `whalerein-debug` profile is created and the checkout installed
# with `dsh plugin` when missing. Existing settings are preserved unless an
# explicit --managed/--standalone mode is given. The launcher does not touch
# Access, PostgreSQL or the broker; the plugin reconnects when a control plane
# appears.
#
# Options (environment fallbacks in parentheses):
#   --managed | --standalone   seed or rewrite the plugin entry
#   --profile NAME             (WHALEREIN_DSH_PROFILE, default whalerein-debug)
#   --port PORT                (WHALEREIN_DSH_PORT, default 3099)
#   --reinstall                force `dsh plugin add` of this checkout
#   --no-developer-tools       hide Developer tools in the managed profile
#   --log-level LEVEL          (WHALEREIN_LOG_LEVEL, default info)
#   --dry-run                  print the planned entry and command
#   -h, --help
#
# Other launch overrides:
#   WHALEREIN_DATA_DIR         default <repo>/.data
#   WHALEREIN_CONFIG           optional explicit per-device TOML path
#   WHALEREIN_HOST_ID          used when seeding a fresh standalone entry
#   WHALEREIN_HOST_TOKEN       default dev-host-token (standalone only)
#   WHALEREIN_ADMIN_TOKEN      default dev-admin-token (standalone only)
#   WHALEREIN_STATE_DIR        standalone SQLite dir
#   WHALEREIN_STATE_ROOT       managed state root
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DATA_DIR="${WHALEREIN_DATA_DIR:-$ROOT/.data}"
PORT="${WHALEREIN_DSH_PORT:-3099}"
PROFILE="${WHALEREIN_DSH_PROFILE:-whalerein-debug}"
HOST_TOKEN="${WHALEREIN_HOST_TOKEN:-dev-host-token}"
ADMIN_TOKEN="${WHALEREIN_ADMIN_TOKEN:-dev-admin-token}"
LOG_LEVEL="${WHALEREIN_LOG_LEVEL:-info}"
MODE=""
MODE_EXPLICIT=0
REINSTALL=0
DRY_RUN=0
DEVELOPER_TOOLS=1

usage() { sed -n '2,31p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --) ;; # tolerated: `pnpm start -- --managed` also works
    --managed) MODE=managed; MODE_EXPLICIT=1 ;;
    --standalone) MODE=standalone; MODE_EXPLICIT=1 ;;
    --profile) PROFILE="${2:?--profile needs a name}"; shift ;;
    --port) PORT="${2:?--port needs a number}"; shift ;;
    --reinstall) REINSTALL=1 ;;
    --no-developer-tools) DEVELOPER_TOOLS=0 ;;
    --log-level) LOG_LEVEL="${2:?--log-level needs a value}"; shift ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

PROFILE_DIR="${DSH_HOME:-$HOME/.dsh}/profiles/$PROFILE"
PATCH_FILE="$PROFILE_DIR/cordis.patch.yml"
if [[ ! "$PORT" =~ ^[0-9]+$ ]]; then
  echo "port must be numeric: $PORT" >&2
  exit 2
fi

entry_value() {
  # Print one scalar from the whalerein-dsh block, or nothing.
  [[ -f "$PATCH_FILE" ]] || return 0
  awk -v key="$1" '
    /^- id: whalerein-dsh$/ {found=1; next}
    found && /^- / {exit}
    found && $1 == key":" {
      sub(/^[^:]*:[[:space:]]*/, "")
      gsub(/^"|"$/, "")
      print
      exit
    }' "$PATCH_FILE"
}

detect_mode() {
  [[ -f "$PATCH_FILE" ]] || return 0
  if [[ -n "$(entry_value connection_mode)" ]]; then
    entry_value connection_mode
  elif [[ -n "$(entry_value server)" || -n "$(entry_value host_token)" ]]; then
    echo standalone
  fi
}

existing_mode="$(detect_mode || true)"
[[ -n "$MODE" ]] || MODE="${existing_mode:-managed}"

STATE_DIR="${WHALEREIN_STATE_DIR:-$(entry_value state_dir)}"
STATE_DIR="${STATE_DIR:-$DATA_DIR/plugin-real-state}"
STATE_ROOT="${WHALEREIN_STATE_ROOT:-$(entry_value state_root)}"
STATE_ROOT="${STATE_ROOT:-$STATE_DIR}"
HOST_ID="${WHALEREIN_HOST_ID:-$(entry_value host_id)}"
if [[ "$MODE" == "standalone" && -z "$HOST_ID" ]]; then
  HOST_ID="$(cd "$ROOT/host/dsh" && node --input-type=module -e "import os from 'node:os'; import { generateHostId } from 'whalerein-host/host-id.js'; process.stdout.write(generateHostId(os.hostname()))")"
fi

if [[ "$DRY_RUN" == 0 ]]; then
  if ! command -v dsh >/dev/null 2>&1; then
    echo "dsh is not on PATH" >&2
    exit 1
  fi
  if command -v lsof >/dev/null 2>&1 && lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "port $PORT is already in use; try: pnpm start --port $((PORT + 1))" >&2
    exit 1
  fi
fi

if [[ ! -f "$PROFILE_DIR/package.json" ]]; then
  echo "creating DSH profile $PROFILE ..."
  if [[ "$DRY_RUN" == 1 ]]; then
    echo "  dsh --profile $PROFILE --from-default-profile web --dump-config"
  else
    dsh --profile "$PROFILE" --from-default-profile web --dump-config >/dev/null
  fi
fi
if [[ "$REINSTALL" == 1 ]] || ! grep -q '"whalerein-dsh"' "$PROFILE_DIR/package.json" 2>/dev/null \
   || [[ ! -e "$PROFILE_DIR/node_modules/whalerein-dsh/package.json" ]]; then
  echo "installing whalerein-dsh into $PROFILE ..."
  if [[ "$DRY_RUN" == 1 ]]; then
    echo "  dsh plugin --profile $PROFILE add $ROOT/host/dsh"
  else
    dsh plugin --profile "$PROFILE" add "$ROOT/host/dsh"
  fi
fi

dev_flag=(--developer-tools)
[[ "$DEVELOPER_TOOLS" == 1 ]] || dev_flag=(--no-developer-tools)

if [[ "$DRY_RUN" == 1 ]]; then
  if [[ -f "$PATCH_FILE" ]] && grep -q 'id: whalerein-dsh' "$PATCH_FILE" && [[ "$MODE_EXPLICIT" == 0 ]]; then
    echo "# existing entry in $PATCH_FILE is preserved (mode: $MODE)"
  else
    print_force=""
    [[ "$MODE_EXPLICIT" == 1 ]] && print_force="--force"
    python3 "$ROOT/host/dsh/bin/profile-patch.py" "$PATCH_FILE" \
      --mode "$MODE" --host-id "$HOST_ID" --state-dir "$STATE_DIR" \
      --state-root "$STATE_ROOT" "${dev_flag[@]}" --log-level "$LOG_LEVEL" --print $print_force
  fi
  echo "# dsh --profile $PROFILE --port $PORT --no-open"
  exit 0
fi

if [[ "$MODE" == "managed" ]]; then
  mkdir -p "$STATE_ROOT"
else
  mkdir -p "$STATE_DIR"
fi

# The settings page writes the profile user layer. Load the Host plugin as a
# bundle (not a launch --patch overlay), otherwise the overlay's environment
# defaults would override saved settings.
python3 - "$PROFILE_DIR/package.json" <<'PY'
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
data = json.loads(path.read_text(encoding="utf-8"))
bundles = data.setdefault("dsh", {}).setdefault("profile", {}).setdefault("bundles", [])
if "whalerein-dsh" not in bundles:
    bundles.append("whalerein-dsh")
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
PY

force=""
[[ "$MODE_EXPLICIT" == 1 ]] && force="--force"
python3 "$ROOT/host/dsh/bin/profile-patch.py" "$PATCH_FILE" \
  --mode "$MODE" --host-id "$HOST_ID" --state-dir "$STATE_DIR" \
  --state-root "$STATE_ROOT" "${dev_flag[@]}" --log-level "$LOG_LEVEL" $force

echo "profile:  $PROFILE ($MODE)"
echo "config:   $PATCH_FILE"
if [[ "$MODE" == "managed" ]]; then
  echo "state:    $STATE_ROOT"
  echo "Developer tools: $([[ "$DEVELOPER_TOOLS" == 1 ]] && echo enabled || echo hidden)"
  echo "Per-device environment configuration: ${WHALEREIN_CONFIG:-~/.whalerein/config.toml}"
else
  echo "state:    $STATE_DIR"
fi
echo "URL:      http://127.0.0.1:$PORT"
echo "Press Ctrl-C to stop."

if [[ "$MODE" == "standalone" ]]; then
  export WHALEREIN_HOST_TOKEN="$HOST_TOKEN"
  export WHALEREIN_ADMIN_TOKEN="$ADMIN_TOKEN"
fi
exec dsh --profile "$PROFILE" --port "$PORT" --no-open
