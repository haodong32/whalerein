#!/usr/bin/env bash
# Adds a Mac Catalyst slice to the generated WhaleReinMobile XCFramework.
#
# The shared apps/mobile-bindings/scripts/build-xcframework.sh invocation
# documented for iOS produces device and Simulator slices only. Mac Catalyst
# needs a fourth slice built for the Rust aarch64-apple-ios-macabi target.
# This script rebuilds only that slice, reuses the generated Swift binding and
# C header from the existing XCFramework, and atomically replaces the generated
# XCFramework. Re-run build-xcframework.sh first when the Rust binding changes.
#
# Usage:
#   bash apps/ios/scripts/build-catalyst-slice.sh
#
# Environment:
#   DEVELOPER_DIR  Defaults to /Applications/Xcode.app/Contents/Developer.
#   PROFILE        Cargo profile, release by default.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${IOS_DIR}/../.." && pwd)"

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="${DEVELOPER_DIR}/usr/bin:${PATH}"
unset SDKROOT

VENDOR_DIR="${IOS_DIR}/Vendor"
XCFRAMEWORK="${VENDOR_DIR}/WhaleReinMobile.xcframework"
FRAMEWORK_NAME="WhaleReinMobileFFI"
CATALYST_TARGET="aarch64-apple-ios-macabi"
PROFILE="${PROFILE:-release}"
MIN_IOS_VERSION="18.0"
WORK_DIR="${REPO_ROOT}/target/ios-catalyst-slice"

if [[ ! -d "${XCFRAMEWORK}" ]]; then
  echo "error: ${XCFRAMEWORK} is missing" >&2
  echo "error: run: bash apps/mobile-bindings/scripts/build-xcframework.sh" >&2
  exit 1
fi
if [[ "${PROFILE}" == "release" ]]; then
  PROFILE_DIR="release"
  PROFILE_FLAG="--release"
elif [[ "${PROFILE}" == "debug" ]]; then
  PROFILE_DIR="debug"
  PROFILE_FLAG="--profile=dev"
else
  PROFILE_DIR="${PROFILE}"
  PROFILE_FLAG="--profile=${PROFILE}"
fi
if ! rustup target list --installed | grep -qx "${CATALYST_TARGET}"; then
  echo "error: Rust target ${CATALYST_TARGET} is not installed" >&2
  echo "error: run: rustup target add ${CATALYST_TARGET}" >&2
  exit 1
fi

DEVICE_FRAMEWORK="${XCFRAMEWORK}/ios-arm64/${FRAMEWORK_NAME}.framework"
SIM_FRAMEWORK=$(find "${XCFRAMEWORK}" -maxdepth 2 -type d -name "${FRAMEWORK_NAME}.framework" \
  -path '*simulator*' | head -n 1)
if [[ ! -d "${DEVICE_FRAMEWORK}" || -z "${SIM_FRAMEWORK}" ]]; then
  echo "error: the existing XCFramework does not contain the expected iOS slices" >&2
  echo "error: run: bash apps/mobile-bindings/scripts/build-xcframework.sh" >&2
  exit 1
fi

echo "Building ${CATALYST_TARGET} (${PROFILE})"
# Match the app's iOS 18 deployment target so the linked objects do not claim a
# newer minimum OS than the Catalyst app.
(cd "${REPO_ROOT}" && MACOSX_DEPLOYMENT_TARGET=15.0 IPHONEOS_DEPLOYMENT_TARGET="${MIN_IOS_VERSION}" \
  cargo build -p whalerein-mobile --lib --no-default-features "${PROFILE_FLAG}" --target "${CATALYST_TARGET}")
CATALYST_LIB="${REPO_ROOT}/target/${CATALYST_TARGET}/${PROFILE_DIR}/libwhalerein_mobile.a"
if [[ ! -f "${CATALYST_LIB}" ]]; then
  echo "error: Catalyst static library not found: ${CATALYST_LIB}" >&2
  exit 1
fi

rm -rf "${WORK_DIR}"
mkdir -p "${WORK_DIR}"
CATALYST_FRAMEWORK_DIR="${WORK_DIR}/${FRAMEWORK_NAME}.framework"
mkdir -p "${CATALYST_FRAMEWORK_DIR}/Headers" "${CATALYST_FRAMEWORK_DIR}/Modules"
cp "${CATALYST_LIB}" "${CATALYST_FRAMEWORK_DIR}/${FRAMEWORK_NAME}"
cp "${DEVICE_FRAMEWORK}/Headers/${FRAMEWORK_NAME}.h" "${CATALYST_FRAMEWORK_DIR}/Headers/"
cp "${DEVICE_FRAMEWORK}/Modules/module.modulemap" "${CATALYST_FRAMEWORK_DIR}/Modules/"

cat > "${CATALYST_FRAMEWORK_DIR}/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>${FRAMEWORK_NAME}</string>
  <key>CFBundleIdentifier</key><string>com.whalerein.mobile.${FRAMEWORK_NAME}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>${FRAMEWORK_NAME}</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>MinimumOSVersion</key><string>${MIN_IOS_VERSION}</string>
  <key>CFBundleSupportedPlatforms</key><array><string>MacOSX</string></array>
  <key>DTPlatformName</key><string>macosx</string>
</dict>
</plist>
PLIST

OUTPUT="${WORK_DIR}/WhaleReinMobile.xcframework"
echo "Creating XCFramework with a Mac Catalyst slice"
xcodebuild -create-xcframework \
  -framework "${DEVICE_FRAMEWORK}" \
  -framework "${SIM_FRAMEWORK}" \
  -framework "${CATALYST_FRAMEWORK_DIR}" \
  -output "${OUTPUT}"

# Replace only after a successful create. The XCFramework is generated and
# gitignored; the previous copy remains until the new one is ready.
OLD="${WORK_DIR}/previous.xcframework"
rm -rf "${OLD}"
mv "${XCFRAMEWORK}" "${OLD}"
mv "${OUTPUT}" "${XCFRAMEWORK}"

plutil -p "${XCFRAMEWORK}/Info.plist" | grep -E 'LibraryIdentifier|SupportedPlatform' || true
echo "Updated ${XCFRAMEWORK}"
