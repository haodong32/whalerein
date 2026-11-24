#!/usr/bin/env bash
# Builds the WhaleRein iOS app for a Simulator destination.
#
# Xcode is not the active developer directory on this host, so DEVELOPER_DIR is
# set explicitly unless the caller already provided one.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
if [[ ! -d "${DEVELOPER_DIR}" ]]; then
  echo "Xcode developer directory not found: ${DEVELOPER_DIR}" >&2
  exit 1
fi

DESTINATION="${DESTINATION:-platform=iOS Simulator,name=iPhone 17,OS=26.4.1}"
CONFIGURATION="${CONFIGURATION:-Debug}"

bash "$IOS_DIR/scripts/generate-project.sh"

echo "Building WhaleRein (${CONFIGURATION}) for ${DESTINATION}"
xcodebuild \
  -project "${IOS_DIR}/WhaleRein.xcodeproj" \
  -scheme WhaleRein \
  -configuration "${CONFIGURATION}" \
  -destination "${DESTINATION}" \
  build \
  "$@"
