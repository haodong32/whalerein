#!/usr/bin/env bash
# Runs one of the Xcode test plans for the WhaleRein iOS app.
#
# Usage:
#   scripts/test.sh [Unit|Integration|UI|all] [destination]
#
# Environment:
#   DEVELOPER_DIR  Defaults to /Applications/Xcode.app/Contents/Developer.
#   DESTINATION    Defaults to the installed iPhone 17 iOS 26.4.1 Simulator.
#   RESULT_BUNDLES Set to 0 to skip writing .xcresult bundles.
#
# Note: this host has the iOS 26.4.1 runtime, not 26.4. The required plan runs
# use OS=26.4.1; see README.md.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${IOS_DIR}/../.." && pwd)"

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
if [[ ! -d "${DEVELOPER_DIR}" ]]; then
  echo "Xcode developer directory not found: ${DEVELOPER_DIR}" >&2
  exit 1
fi

PLAN="${1:-Unit}"
DESTINATION="${2:-${DESTINATION:-platform=iOS Simulator,name=iPhone 17,OS=26.4.1}}"
RESULT_BUNDLES="${RESULT_BUNDLES:-1}"
RESULT_DIR="${RESULT_DIR:-${REPO_ROOT}/.local-docs/acceptance/add-ios-client/xcode}"

run_plan() {
  local plan="$1"
  local result_args=()
  if [[ "${RESULT_BUNDLES}" == "1" ]]; then
    mkdir -p "${RESULT_DIR}"
    local bundle="${RESULT_DIR}/${plan}.result"
    rm -rf "${bundle}"
    result_args=(-resultBundlePath "${bundle}")
  fi
  echo "Testing plan ${plan} on ${DESTINATION}"
  if [[ ${#result_args[@]} -gt 0 ]]; then
    xcodebuild \
      -project "${IOS_DIR}/WhaleRein.xcodeproj" \
      -scheme WhaleRein \
      -destination "${DESTINATION}" \
      -testPlan "${plan}" \
      "${result_args[@]}" \
      test
  else
    xcodebuild \
      -project "${IOS_DIR}/WhaleRein.xcodeproj" \
      -scheme WhaleRein \
      -destination "${DESTINATION}" \
      -testPlan "${plan}" \
      test
  fi
}

case "${PLAN}" in
  Unit|UI)
    run_plan "${PLAN}"
    ;;
  Integration)
    DESTINATION="${DESTINATION}" RESULT_DIR="${RESULT_DIR}" \
      exec "${SCRIPT_DIR}/test-integration.sh"
    ;;
  all)
    run_plan Unit
    "${SCRIPT_DIR}/test-integration.sh"
    run_plan UI
    ;;
  *)
    echo "Unknown test plan: ${PLAN} (expected Unit, Integration, UI, or all)" >&2
    exit 2
    ;;
esac
