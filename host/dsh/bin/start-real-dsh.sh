#!/usr/bin/env bash
# Start real DSH with the WhaleRein plugin loaded, then stay in the foreground
# until Ctrl-C.
#
#   cd host/dsh
#   pnpm start
#
# It creates the dedicated `whalerein-debug` profile if needed and installs
# this package with `dsh plugin` (pnpm). It does not touch Access, PostgreSQL,
# or EMQX; the plugin reconnects when a control plane appears.
#
# Launch overrides:
#   WHALEREIN_DSH_PORT           default 3099
#   WHALEREIN_DSH_PROFILE        default whalerein-debug
#   WHALEREIN_CONFIG             optional explicit per-device TOML path
#   WHALEREIN_STATE_DIR          used only for the first managed state seed
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DATA_DIR="${WHALEREIN_DATA_DIR:-$ROOT/.data}"
PORT="${WHALEREIN_DSH_PORT:-3099}"
PROFILE="${WHALEREIN_DSH_PROFILE:-whalerein-debug}"
PROFILE_DIR="${DSH_HOME:-$HOME/.dsh}/profiles/$PROFILE"
STATE_DIR="${WHALEREIN_STATE_DIR:-$DATA_DIR/plugin-real-state}"

if ! command -v dsh >/dev/null 2>&1; then
  echo "dsh is not on PATH" >&2
  exit 1
fi

if [[ ! -f "$PROFILE_DIR/package.json" ]]; then
  echo "creating DSH profile $PROFILE ..."
  dsh --profile "$PROFILE" --from-default-profile web --dump-config >/dev/null
fi
# Reinstall a missing package or a checkout link broken by a repository move.
if ! grep -q '"whalerein-dsh"' "$PROFILE_DIR/package.json" || [[ ! -f "$PROFILE_DIR/node_modules/whalerein-dsh/package.json" ]]; then
  echo "installing whalerein-dsh into $PROFILE with pnpm ..."
  dsh plugin --profile "$PROFILE" add "$ROOT/host/dsh"
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

mkdir -p "$STATE_DIR"

# First launch seeds one managed entry with no endpoint or trust material. Later
# launches leave that file alone; the per-device TOML file and settings page are
# the editors.
python3 "$ROOT/host/dsh/bin/profile-patch.py" "$PROFILE_DIR/cordis.patch.yml" "$STATE_DIR"

echo "starting real DSH: profile=$PROFILE port=$PORT"
echo "Connection settings are in $PROFILE_DIR/cordis.patch.yml."
echo "Per-device environment configuration: ${WHALEREIN_CONFIG:-~/.whalerein/config.toml}"
echo "Open the URL printed below on port $PORT; the existing 3080 DSH is a separate instance."
echo "Press Ctrl-C to stop."
exec dsh --profile "$PROFILE" --port "$PORT" --no-open
