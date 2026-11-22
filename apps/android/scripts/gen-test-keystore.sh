#!/usr/bin/env bash
#
# Generate a repository-local Android test signing keystore. The output stays
# under apps/android/keys/ (git-ignored) and is only for local install/upgrade
# acceptance. Never use it for a published artifact.
#
# Usage:
#   apps/android/scripts/gen-test-keystore.sh [output.jks]
#
# Environment:
#   JAVA_HOME              JDK 21 home (keytool is looked up there first)
#   KEYSTORE_PASSWORD      test store password (default: whalerein-test)
#   KEY_PASSWORD           test key password (default: same as store)
#   KEY_ALIAS              key alias (default: whalerein-test)
#   FORCE=1                overwrite an existing keystore

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ANDROID_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
OUTPUT=${1:-"$ANDROID_DIR/keys/test-release.jks"}
STORE_PASSWORD=${KEYSTORE_PASSWORD:-whalerein-test}
KEY_PASSWORD=${KEY_PASSWORD:-$STORE_PASSWORD}
ALIAS=${KEY_ALIAS:-whalerein-test}
DNAME=${KEY_DNAME:-"CN=WhaleRein Local Test, OU=Engineering, O=WhaleRein, L=Local, ST=Local, C=US"}

KEYTOOL=keytool
if [[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/keytool" ]]; then
  KEYTOOL="$JAVA_HOME/bin/keytool"
fi

if [[ -e "$OUTPUT" && "${FORCE:-0}" != 1 ]]; then
  printf 'error: %s exists; set FORCE=1 to replace it\n' "$OUTPUT" >&2
  exit 1
fi
mkdir -p "$(dirname "$OUTPUT")"
rm -f "$OUTPUT"

"$KEYTOOL" -genkeypair \
  -keystore "$OUTPUT" \
  -storetype PKCS12 \
  -alias "$ALIAS" \
  -keyalg RSA \
  -keysize 4096 \
  -validity 10000 \
  -storepass "$STORE_PASSWORD" \
  -keypass "$KEY_PASSWORD" \
  -dname "$DNAME"

printf '\nTest keystore: %s\n' "$OUTPUT"
printf 'The file is git-ignored. Build a signed release with:\n\n'
printf '  export WHALEREIN_ANDROID_KEYSTORE=%q\n' "$OUTPUT"
printf '  export WHALEREIN_ANDROID_KEYSTORE_PASSWORD=%q\n' "$STORE_PASSWORD"
printf '  export WHALEREIN_ANDROID_KEY_ALIAS=%q\n' "$ALIAS"
printf '  export WHALEREIN_ANDROID_KEY_PASSWORD=%q\n' "$KEY_PASSWORD"
printf '  cd apps/android && ./gradlew :app:assembleNoGmsRelease\n'
