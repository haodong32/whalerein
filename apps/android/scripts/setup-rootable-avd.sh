#!/usr/bin/env bash
#
# Prepare a rootable emulator for local Cloud (`*.whalerein.test`) device
# acceptance: system trust for the mkcert development CA and guest host entries
# that point the local authorities at the host loopback (10.0.2.2).
#
# API 33 is the default because it still uses the legacy
# /system/etc/security/cacerts store, which is writable with -writable-system.
# Android 14+ keeps CAs in the Conscrypt APEX; the script attempts a bind-mount
# overlay, but app/zygote mount namespaces may not see it. API 36/37 system
# images additionally failed to boot on the recorded host (`mprotect failed`).
#
# Chrome still labels `*.test` "identity not verified" because `.test` is a
# special-use TLD even when the CA is trusted; use a real domain or accept that
# warning for local browser checks.
#
# This is a local test tool. It never runs against a physical device and does
# not change release artifacts. The Android 14+ CA store lives in the Conscrypt
# APEX, so the script bind-mounts a prepared copy over it for the session; a
# reboot loses that bind mount and this script must be re-run.
#
# Usage:
#   apps/android/scripts/setup-rootable-avd.sh [avd-name]
#
# Environment:
#   ANDROID_HOME / ANDROID_SDK_ROOT   SDK root (default ~/Library/Android/sdk)
#   WHALEREIN_ANDROID_AVD             AVD name (default wr_api33_root)
#   WHALEREIN_AVD_IMAGE               system image (default android-33 google_apis)
#   WHALEREIN_DEV_CA                  mkcert root CA PEM
#   WHALEREIN_AVD_SERIAL              adb serial when more than one device
#
# Requires a `google_apis` (non-Play) image and an already booted AVD started
# with `-writable-system`. The default image is API 33 because its system CA
# store is writable; see the header note about newer images.

set -euo pipefail

AVD=${1:-${WHALEREIN_ANDROID_AVD:-wr_api33_root}}
SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-"$HOME/Library/Android/sdk"}}
ADB="$SDK/platform-tools/adb"
EMULATOR="$SDK/emulator/emulator"
CA=${WHALEREIN_DEV_CA:-"$HOME/Library/Application Support/mkcert/rootCA.pem"}
HOSTS=(app.whalerein.test auth.whalerein.test access.whalerein.test admin.whalerein.test mail.whalerein.test)

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[[ -x "$ADB" ]] || die "adb not found under $SDK"
[[ -f "$CA" ]] || die "mkcert root CA not found: $CA (run mkcert -install)"

if ! "$EMULATOR" -list-avds | grep -qx "$AVD"; then
  IMAGE=${WHALEREIN_AVD_IMAGE:-"system-images;android-33;google_apis;arm64-v8a"}
  printf 'AVD %s not found; creating it from %s\n' "$AVD" "$IMAGE"
  echo no | "$SDK/cmdline-tools/latest/bin/avdmanager" create avd \
    -n "$AVD" \
    -k "$IMAGE" \
    -d pixel_9 --force
fi

if [[ -n "${WHALEREIN_AVD_SERIAL:-}" ]]; then
  serial=$WHALEREIN_AVD_SERIAL
else
  serial=$("$ADB" devices | awk 'NR>1 && $2=="device"{print $1; exit}')
fi
[[ -n "$serial" ]] || {
  printf 'No booted device. Start it first:\n\n  %s -avd %s -writable-system -no-snapshot -no-window &\n\n' "$EMULATOR" "$AVD" >&2
  exit 2
}

printf 'Using device %s\n' "$serial"
adb=( "$ADB" -s "$serial" )
"${adb[@]}" root >/dev/null 2>&1 || true
sleep 2
# `adb root` restarts adbd; wait for it before remounting.
for _ in $(seq 1 30); do
  "${adb[@]}" shell true >/dev/null 2>&1 && break
  sleep 1
done

sdk=$("${adb[@]}" shell getprop ro.build.version.sdk | tr -d '\r')
printf 'Guest API level: %s\n' "$sdk"

"${adb[@]}" remount >/dev/null 2>&1 || true
if ! "${adb[@]}" shell 'test -w /system/etc/hosts' >/dev/null 2>&1; then
  printf 'Remounting /system read-write...\n'
  "${adb[@]}" disable-verity >/dev/null 2>&1 || true
  "${adb[@]}" reboot || true
  sleep 8
  "${adb[@]}" wait-for-device
  "${adb[@]}" root >/dev/null 2>&1 || true
  sleep 2
  "${adb[@]}" remount >/dev/null 2>&1 || true
fi
"${adb[@]}" shell 'test -w /system/etc/hosts' >/dev/null 2>&1 \
  || die "system partition is not writable; start the AVD with -writable-system and rerun"

printf 'Writing guest hosts entries...\n'
"${adb[@]}" shell 'grep -q "app.whalerein.test" /system/etc/hosts 2>/dev/null || true'
tmp_hosts=$(mktemp)
"${adb[@]}" shell cat /system/etc/hosts > "$tmp_hosts" 2>/dev/null || true
for host in "${HOSTS[@]}"; do
  grep -qE "[[:space:]]${host}([[:space:]]|$)" "$tmp_hosts" || echo "10.0.2.2 $host" >> "$tmp_hosts"
done
"${adb[@]}" push "$tmp_hosts" /data/local/tmp/whalerein-hosts >/dev/null
"${adb[@]}" shell cp /data/local/tmp/whalerein-hosts /system/etc/hosts
"${adb[@]}" shell chmod 644 /system/etc/hosts
rm -f "$tmp_hosts"

hash=$(openssl x509 -subject_hash_old -in "$CA" -noout | head -n 1)
printf 'Installing CA %s as system trust (%s.0)...\n' "$CA" "$hash"

# Legacy location used before Android 14.
"${adb[@]}" push "$CA" "/data/local/tmp/$hash.0" >/dev/null
if "${adb[@]}" shell 'test -d /system/etc/security/cacerts' >/dev/null 2>&1; then
  "${adb[@]}" shell cp "/data/local/tmp/$hash.0" "/system/etc/security/cacerts/$hash.0"
  "${adb[@]}" shell chmod 644 "/system/etc/security/cacerts/$hash.0"
fi

# Android 14+ reads CA certificates from the Conscrypt APEX. Overlay a copy
# that includes the development CA for this boot.
if "${adb[@]}" shell 'test -d /apex/com.android.conscrypt/cacerts' >/dev/null 2>&1; then
  "${adb[@]}" shell 'rm -rf /data/local/tmp/cacerts && mkdir -p /data/local/tmp/cacerts'
  "${adb[@]}" shell 'cp /apex/com.android.conscrypt/cacerts/* /data/local/tmp/cacerts/ 2>/dev/null || true'
  "${adb[@]}" shell cp "/data/local/tmp/$hash.0" "/data/local/tmp/cacerts/$hash.0"
  "${adb[@]}" shell 'chmod 644 /data/local/tmp/cacerts/*'
  "${adb[@]}" shell 'chcon u:object_r:system_file:s0 /data/local/tmp/cacerts/* 2>/dev/null || true'
  if "${adb[@]}" shell 'mount --bind /data/local/tmp/cacerts /apex/com.android.conscrypt/cacerts' 2>/dev/null; then
    printf 'Conscrypt APEX CA store overlaid for this boot.\n'
  else
    printf 'warning: could not bind-mount the Conscrypt APEX CA store; Chrome/App Links may still reject the dev CA\n' >&2
  fi
fi

printf '\nVerification:\n'
"${adb[@]}" shell 'getprop ro.build.version.sdk; getconf PAGE_SIZE; grep whalerein.test /system/etc/hosts; ls /apex/com.android.conscrypt/cacerts/'"$hash"'.0 2>/dev/null || true'
printf '\nOpen https://app.whalerein.test/ in the guest browser to confirm trust.\n'
