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
#   WHALEREIN_HOST_ID            used only when the profile has no WhaleRein config yet
#   WHALEREIN_HOST_TOKEN         default dev-host-token; exported because the seeded
#                                profile stores host_token as an env expression
#   WHALEREIN_ADMIN_TOKEN        default dev-admin-token; same, for the seeded admin token
#   WHALEREIN_STATE_DIR          used only for the first seed
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DATA_DIR="${WHALEREIN_DATA_DIR:-$ROOT/.data}"
PORT="${WHALEREIN_DSH_PORT:-3099}"
PROFILE="${WHALEREIN_DSH_PROFILE:-whalerein-debug}"
PROFILE_DIR="${DSH_HOME:-$HOME/.dsh}/profiles/$PROFILE"
HOST_TOKEN="${WHALEREIN_HOST_TOKEN:-dev-host-token}"
ADMIN_TOKEN="${WHALEREIN_ADMIN_TOKEN:-dev-admin-token}"
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

# A missing profile entry gets one host id. An existing entry, including a saved
# host-real, is left alone. WHALEREIN_HOST_ID is only that first seed.
NEEDS_HOST_SEED=0
if [[ ! -f "$PROFILE_DIR/cordis.patch.yml" ]] || ! grep -q 'id: whalerein-dsh' "$PROFILE_DIR/cordis.patch.yml"; then
  NEEDS_HOST_SEED=1
fi
HOST_ID=""
if [[ "$NEEDS_HOST_SEED" == 1 ]]; then
  if [[ -n "${WHALEREIN_HOST_ID:-}" ]]; then
    HOST_ID="$WHALEREIN_HOST_ID"
  else
    HOST_ID="$(cd "$ROOT/host/dsh" && node --input-type=module -e "import os from 'node:os'; import { generateHostId } from 'whalerein-host/host-id.js'; process.stdout.write(generateHostId(os.hostname()))")"
  fi
fi

# First launch writes the local Compose defaults into the profile. Later launches
# leave that file alone; the settings page is the editor.
python3 "$ROOT/host/dsh/bin/profile-patch.py" "$PROFILE_DIR/cordis.patch.yml" "$HOST_ID" "$STATE_DIR"

if [[ -n "$HOST_ID" ]]; then
  echo "starting real DSH: profile=$PROFILE host=$HOST_ID port=$PORT"
else
  echo "starting real DSH: profile=$PROFILE port=$PORT"
fi
echo "Connection settings are in $PROFILE_DIR/cordis.patch.yml."
echo "Open the URL printed below on port $PORT; the existing 3080 DSH is a separate instance."
echo "Press Ctrl-C to stop."
exec env \
  WHALEREIN_HOST_TOKEN="$HOST_TOKEN" \
  WHALEREIN_ADMIN_TOKEN="$ADMIN_TOKEN" \
  dsh --profile "$PROFILE" --port "$PORT"
