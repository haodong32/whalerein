#!/usr/bin/env bash
# Runs the Integration test plan against the real Swift -> UniFFI -> Rust Core
# path on the disposable loopback stack.
#
# Usage:
#   apps/ios/scripts/test-integration.sh
#
# Environment:
#   DEVELOPER_DIR   Defaults to /Applications/Xcode.app/Contents/Developer.
#   DESTINATION     Defaults to the installed iPhone 17 iOS 26.4.1 Simulator.
#   KEEP_STACK=1    Leaves the stack running after the test run.
#   RESULT_DIR      Defaults to .local-docs/acceptance/add-ios-client/xcode.
#   DERIVED_DATA    Defaults to .data/ios-derived/integration.
#   ONLY_TESTING    Optional xcodebuild -only-testing selector for iteration.
#
# The script never prints invitation URIs, control tokens or message content.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${IOS_DIR}/../.." && pwd)"
STACK_TOOL="${REPO_ROOT}/tools/ios-stack/ios_stack.py"
STATE_PATH="${REPO_ROOT}/.data/ios-stack/current/state.json"

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
if [[ ! -d "${DEVELOPER_DIR}" ]]; then
  echo "Xcode developer directory not found: ${DEVELOPER_DIR}" >&2
  exit 1
fi

DESTINATION="${DESTINATION:-platform=iOS Simulator,name=iPhone 17,OS=26.4.1}"
RESULT_DIR="${RESULT_DIR:-${REPO_ROOT}/.local-docs/acceptance/add-ios-client/xcode}"
RESULT_BUNDLE="${RESULT_DIR}/Integration.result"
DERIVED_DATA="${DERIVED_DATA:-${REPO_ROOT}/.data/ios-derived/integration}"
KEEP_STACK="${KEEP_STACK:-0}"

if [[ ! -d "${IOS_DIR}/Vendor/WhaleReinMobile.xcframework" || \
      ! -f "${IOS_DIR}/Vendor/WhaleReinMobile/WhaleReinMobile.swift" || \
      ! -f "${IOS_DIR}/Vendor/WhaleReinMobile/WhaleReinMobileError.swift" || \
      ! -f "${IOS_DIR}/Vendor/revision.txt" ]]; then
  echo "error: apps/ios/Vendor is missing the generated binding." >&2
  echo "error: run: bash apps/mobile-bindings/scripts/build-xcframework.sh" >&2
  exit 1
fi

cleanup() {
  local status=$?
  if [[ "${KEEP_STACK}" == "1" ]]; then
    echo "KEEP_STACK=1: leaving the iOS stack running."
    echo "Stop it with: python3 tools/ios-stack/ios_stack.py down"
    exit "${status}"
  fi
  echo "Stopping the iOS stack..."
  python3 "${STACK_TOOL}" down >/dev/null 2>&1 || true
  exit "${status}"
}
trap cleanup EXIT INT TERM

stack_ready() {
  python3 "${STACK_TOOL}" status --json >/dev/null 2>&1
}

echo "Ensuring the iOS loopback stack is up..."
if ! stack_ready; then
  if ! python3 "${STACK_TOOL}" up >/dev/null 2>"${TMPDIR:-/tmp}/whalerein-ios-stack-up.log"; then
    echo "error: the iOS stack failed to start. Last lines:" >&2
    tail -n 10 "${TMPDIR:-/tmp}/whalerein-ios-stack-up.log" >&2 || true
    exit 1
  fi
fi
if ! stack_ready; then
  echo "error: the iOS stack is not ready after startup." >&2
  exit 1
fi

# Mint a fresh one-time invitation. It stays in shell variables and the
# generated xctestrun only; it is never echoed or written to the evidence log.
echo "Minting a fresh invitation..."
INVITATION="$(python3 "${STACK_TOOL}" invitation --refresh 2>/dev/null)"
if [[ -z "${INVITATION}" ]]; then
  echo "error: could not mint a stack invitation." >&2
  exit 1
fi

read_contract_field() {
  python3 - "${STATE_PATH}" "$1" <<'PY'
import json
import sys

path, field = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    value = json.load(handle)
current = value
for part in field.split("."):
    current = current[part]
print(current)
PY
}

ACCESS_URL="$(read_contract_field access_url)"
MQTT_URL="$(read_contract_field mqtt_url)"
CONTROL_URL="$(read_contract_field control_url)"
CONTROL_TOKEN="$(read_contract_field control_token)"
HOST_ID="$(read_contract_field host_id)"
SESSION_ID="$(read_contract_field session_id)"
TEARDOWN_MARKER="$(read_contract_field teardown_marker)"
EXPECTED_REVISION="$(cat "${IOS_DIR}/Vendor/revision.txt")"

echo "Building the Integration plan for testing..."
rm -rf "${DERIVED_DATA}"
xcodebuild \
  -project "${IOS_DIR}/WhaleRein.xcodeproj" \
  -scheme WhaleRein \
  -destination "${DESTINATION}" \
  -testPlan Integration \
  -derivedDataPath "${DERIVED_DATA}" \
  build-for-testing

XCTESTRUN="$(find "${DERIVED_DATA}/Build/Products" -name '*.xctestrun' -type f | head -n 1)"
if [[ -z "${XCTESTRUN}" ]]; then
  echo "error: build-for-testing did not produce an .xctestrun file." >&2
  exit 1
fi

echo "Injecting stack configuration into the test runner environment..."
WHALEREIN_IOS_STACK_ACCESS_URL="${ACCESS_URL}" \
WHALEREIN_IOS_STACK_MQTT_URL="${MQTT_URL}" \
WHALEREIN_IOS_STACK_INVITATION_URI="${INVITATION}" \
WHALEREIN_IOS_STACK_CONTROL_URL="${CONTROL_URL}" \
WHALEREIN_IOS_STACK_CONTROL_TOKEN="${CONTROL_TOKEN}" \
WHALEREIN_IOS_STACK_HOST_ID="${HOST_ID}" \
WHALEREIN_IOS_STACK_SESSION_ID="${SESSION_ID}" \
WHALEREIN_IOS_STACK_TEARDOWN_MARKER="${TEARDOWN_MARKER}" \
WHALEREIN_IOS_STACK_EXPECTED_REVISION="${EXPECTED_REVISION}" \
python3 - "${XCTESTRUN}" <<'PY'
import os
import plistlib
import sys

path = sys.argv[1]
keys = [
    "WHALEREIN_IOS_STACK_ACCESS_URL",
    "WHALEREIN_IOS_STACK_MQTT_URL",
    "WHALEREIN_IOS_STACK_INVITATION_URI",
    "WHALEREIN_IOS_STACK_CONTROL_URL",
    "WHALEREIN_IOS_STACK_CONTROL_TOKEN",
    "WHALEREIN_IOS_STACK_HOST_ID",
    "WHALEREIN_IOS_STACK_SESSION_ID",
    "WHALEREIN_IOS_STACK_TEARDOWN_MARKER",
    "WHALEREIN_IOS_STACK_EXPECTED_REVISION",
]
environment = {key: os.environ[key] for key in keys}

with open(path, "rb") as handle:
    document = plistlib.load(handle)

def apply(target):
    existing = target.get("EnvironmentVariables", {})
    existing.update(environment)
    target["EnvironmentVariables"] = existing

if "TestConfigurations" in document:
    for configuration in document["TestConfigurations"]:
        for target in configuration.get("TestTargets", []):
            apply(target)
elif "TestTargets" in document:
    for target in document["TestTargets"]:
        apply(target)
else:
    raise SystemExit("unrecognized xctestrun layout")

with open(path, "wb") as handle:
    plistlib.dump(document, handle)
PY

mkdir -p "${RESULT_DIR}"
rm -rf "${RESULT_BUNDLE}" "${RESULT_BUNDLE}.xcresult"
echo "Running the Integration plan..."
set +e
if [[ -n "${ONLY_TESTING:-}" ]]; then
  xcodebuild \
    test-without-building \
    -xctestrun "${XCTESTRUN}" \
    -destination "${DESTINATION}" \
    -resultBundlePath "${RESULT_BUNDLE}.xcresult" \
    -only-testing "${ONLY_TESTING}"
else
  xcodebuild \
    test-without-building \
    -xctestrun "${XCTESTRUN}" \
    -destination "${DESTINATION}" \
    -resultBundlePath "${RESULT_BUNDLE}.xcresult"
fi
STATUS=$?
set -e
if [[ -d "${RESULT_BUNDLE}.xcresult" ]]; then
  rm -rf "${RESULT_BUNDLE}"
  mv "${RESULT_BUNDLE}.xcresult" "${RESULT_BUNDLE}"
fi

echo "Integration result bundle: ${RESULT_BUNDLE}"
exit "${STATUS}"
