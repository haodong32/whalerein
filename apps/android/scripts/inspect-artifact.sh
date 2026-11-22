#!/usr/bin/env bash
#
# Inspect an Android APK or AAB for native libraries, 16 KB page alignment and
# package metadata. Read-only: the artifact is never modified.
#
# Usage:
#   apps/android/scripts/inspect-artifact.sh <path.apk|path.aab>
#
# Environment:
#   ANDROID_SDK_ROOT / ANDROID_HOME   Android SDK (default ~/Library/Android/sdk)
#   ANDROID_BUILD_TOOLS               build-tools version (default: newest found)
#   ANDROID_NDK                       NDK directory (default: $SDK/ndk/28.2.13676358)

set -euo pipefail

if [[ $# -ne 1 ]]; then
  printf 'usage: %s <artifact.apk|artifact.aab>\n' "$(basename "$0")" >&2
  exit 2
fi
ARTIFACT=$1
[[ -f "$ARTIFACT" ]] || { printf 'error: artifact not found: %s\n' "$ARTIFACT" >&2; exit 2; }

SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-"$HOME/Library/Android/sdk"}}
BUILD_TOOLS=${ANDROID_BUILD_TOOLS:-$(ls "$SDK/build-tools" 2>/dev/null | sort -V | tail -n 1)}
NDK=${ANDROID_NDK:-"$SDK/ndk/28.2.13676358"}
AAPT2="$SDK/build-tools/$BUILD_TOOLS/aapt2"
ZIPALIGN="$SDK/build-tools/$BUILD_TOOLS/zipalign"
READELF="$NDK/toolchains/llvm/prebuilt/$(uname -s | tr '[:upper:]' '[:lower:]')-x86_64/bin/llvm-readelf"
case "$(uname -s)" in
  Darwin) HOST_TAG=darwin-x86_64 ;;
  Linux) HOST_TAG=linux-x86_64 ;;
  *) HOST_TAG=windows-x86_64 ;;
esac
READELF="$NDK/toolchains/llvm/prebuilt/$HOST_TAG/bin/llvm-readelf"

printf 'Artifact: %s\n' "$ARTIFACT"
printf 'Size: %s bytes\n' "$(wc -c < "$ARTIFACT" | tr -d ' ')"
printf 'SHA-256: %s\n' "$(shasum -a 256 "$ARTIFACT" | awk '{print $1}')"
printf 'Build tools: %s\n' "$BUILD_TOOLS"
printf 'NDK: %s\n' "$NDK"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

case "$ARTIFACT" in
  *.apk)
    printf '\n== Package metadata ==\n'
    "$AAPT2" dump badging "$ARTIFACT" | sed -n \
      -e 's/^package: /package: /p' \
      -e 's/^sdkVersion:/minSdk:/p' \
      -e 's/^targetSdkVersion:/targetSdk:/p' \
      -e 's/^native-code: /native-code: /p'
    printf '\n== Native libraries ==\n'
    unzip -l "$ARTIFACT" | awk '/\.so$/ {print "  " $1 "  " $4}'
    printf '\n== zipalign (-c -P 16 -v 4) ==\n'
    "$ZIPALIGN" -c -P 16 -v 4 "$ARTIFACT" | tail -n 20
    unzip -q "$ARTIFACT" 'lib/*' -d "$TMP" || true
    ;;
  *.aab)
    printf '\n== Bundle contents ==\n'
    unzip -l "$ARTIFACT" | awk '/\.so$/ || /base\/manifest/ || /BundleConfig/ {print "  " $1 "  " $4}'
    METADATA=$(dirname "$ARTIFACT")/output-metadata.json
    printf '\n== Package metadata ==\n'
    if [[ -f "$METADATA" ]]; then
      printf '  source: %s\n' "$METADATA"
      grep -E '"(applicationId|versionCode|versionName)"' "$METADATA" | sed 's/^/  /' || true
    elif command -v bundletool >/dev/null 2>&1; then
      bundletool dump manifest --bundle "$ARTIFACT" 2>/dev/null | sed -n '1,20p' || true
    else
      # AAB stores a protobuf manifest; recover the scalar attributes from its
      # embedded UTF-8 strings without extra tooling.
      unzip -p "$ARTIFACT" base/manifest/AndroidManifest.xml > "$TMP/AndroidManifest.pb"
      package=$(strings "$TMP/AndroidManifest.pb" |
        awk '/^package$/{getline; while ($0 ~ /^\*/) getline; print; exit}' |
        cut -d'"' -f1 | tr -cd 'A-Za-z0-9._-')
      version_name=$(strings "$TMP/AndroidManifest.pb" |
        awk '/^versionName$/{getline; while ($0 !~ /^[0-9]/) getline; print; exit}' |
        tr -cd 'A-Za-z0-9._+\-')
      printf '  package: %s\n  versionName: %s\n  versionCode: (binary; inspect the APK or BundleConfig)\n' \
        "${package:-unknown}" "${version_name:-unknown}"
    fi
    printf '\n== Native libraries ==\n'
    unzip -l "$ARTIFACT" | awk '/base\/lib\/.*\.so$/ {print "  " $1 "  " $4}'
    unzip -q "$ARTIFACT" 'base/lib/*' -d "$TMP" || true
    ;;
  *)
    printf 'error: unsupported artifact type (expected .apk or .aab)\n' >&2
    exit 2
    ;;
esac

printf '\n== 16 KB ELF segment alignment ==\n'
found=0
while IFS= read -r library; do
  found=1
  printf '%s:\n' "${library#"$TMP"/}"
  aligns=$("$READELF" -lW "$library" | awk '$1 == "LOAD" {print $NF}' | sort -u)
  under=0
  while IFS= read -r align; do
    [[ -z "$align" ]] && continue
    if (( align < 16384 )); then
      under=$((under + 1))
    fi
  done <<< "$aligns"
  printf '  LOAD align(s): %s\n' "$(tr '\n' ' ' <<< "$aligns")"
  if [[ "$under" == 0 ]]; then
    printf '  16 KB compatible: yes\n'
  else
    printf '  16 KB compatible: NO (%s LOAD segment(s) below 0x4000)\n' "$under"
  fi
done < <(find "$TMP" -name '*.so' -type f | sort)
if [[ "$found" == 0 ]]; then
  printf '  no packaged native libraries found\n'
fi
