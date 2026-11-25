#!/usr/bin/env bash
# Verifies that the Debug state gallery and the scenario catalog are absent
# from a Release-configured product.
#
# Fails when the Release binary or its bundled strings contain a gallery
# identifier, the scenario launch flag, or the Debug surface marker.

set -euo pipefail

IOS_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
DERIVED="${DERIVED_DATA:-${IOS_DIR}/.release-scan}"
DESTINATION="${DESTINATION:-platform=iOS Simulator,name=iPhone 17 Pro}"

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"

bash "$IOS_DIR/scripts/generate-project.sh"

BUILD_LOG="${DERIVED}/release-scan-build.log"
mkdir -p "${DERIVED}"
if ! xcodebuild \
  -project "${IOS_DIR}/WhaleRein.xcodeproj" \
  -scheme WhaleRein \
  -configuration Release \
  -destination "${DESTINATION}" \
  -derivedDataPath "${DERIVED}" \
  ARCHS=arm64 \
  build >"${BUILD_LOG}" 2>&1; then
  tail -n 120 "${BUILD_LOG}" >&2
  exit 1
fi

APP="${DERIVED}/Build/Products/Release-iphonesimulator/WhaleRein.app"
BINARY="${APP}/WhaleRein"
if [[ ! -f "${BINARY}" ]]; then
  echo "Release binary not found at ${BINARY}" >&2
  exit 1
fi

# Markers that only exist when the Debug surface is compiled in.
PATTERN='gallery\.current|gallery\.scenario|conversation\.gallery|-ui-scenario|State gallery|ScenarioCatalog'

status=0
if strings -a "${BINARY}" | grep -Eq "${PATTERN}"; then
  echo "Debug surface leaked into the Release binary:" >&2
  strings -a "${BINARY}" | grep -E "${PATTERN}" | head -10 >&2
  status=1
fi

# Bundled resources must not carry the catalog either.
if grep -RIlEq "${PATTERN}" "${APP}" --include='*.json' --include='*.plist' 2>/dev/null; then
  echo "Debug surface leaked into a bundled resource:" >&2
  grep -RIlE "${PATTERN}" "${APP}" --include='*.json' --include='*.plist' | head -10 >&2
  status=1
fi

if [[ "${status}" -eq 0 ]]; then
  echo "Release surface scan: OK (no gallery or scenario markers)"
fi
exit "${status}"
