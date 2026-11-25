#!/usr/bin/env bash
#
# Build the shared WhaleReinMobile XCFramework and Swift bindings for the iOS
# app. Device and Simulator slices are built from the same working tree; Swift
# bindings are generated from a host library built in the same run so the
# generated sources and the linked slices share one crate revision.
#
# Usage:
#   bash apps/mobile-bindings/scripts/build-xcframework.sh
#
# Environment:
#   PROFILE=release|debug|<cargo profile>   build profile (default: release)
#   SIM_TARGETS="aarch64-apple-ios-sim x86_64-apple-ios"
#                                           simulator slices (default: arm64)
#   WHALEREIN_MOBILE_SIMULATOR_ONLY=1        omit the device slice for tests
#   DEVELOPER_DIR                           Xcode developer directory; when
#                                           unset, /Applications/Xcode.app is
#                                           used if it exists.
#
# Output (all generated, gitignored):
#   apps/ios/Vendor/WhaleReinMobile.xcframework
#   apps/ios/Vendor/WhaleReinMobile/WhaleReinMobile.swift
#   apps/ios/Vendor/WhaleReinMobile/WhaleReinMobileError.swift
#   apps/ios/Vendor/WhaleReinMobile/WhaleReinMobile.revision
#   apps/ios/Vendor/revision.txt

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
CRATE_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
ROOT=$(cd -- "$CRATE_DIR/../.." && pwd)

IOS_DIR="$ROOT/apps/ios"
VENDOR_DIR="$IOS_DIR/Vendor"
SWIFT_VENDOR_DIR="$VENDOR_DIR/WhaleReinMobile"
XCFRAMEWORK="$VENDOR_DIR/WhaleReinMobile.xcframework"
BUILD_ROOT="$ROOT/target/mobile-bindings-xcframework"
BINDINGS_DIR="$BUILD_ROOT/bindings"
CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT/target"}
PROFILE=${PROFILE:-release}
SIM_TARGETS=${SIM_TARGETS:-"aarch64-apple-ios-sim"}
SIMULATOR_ONLY=${WHALEREIN_MOBILE_SIMULATOR_ONLY:-0}

DEVICE_TARGET="aarch64-apple-ios"
FRAMEWORK_NAME="WhaleReinMobileFFI"
MIN_IOS_VERSION="18.0"

if [[ -z "${DEVELOPER_DIR:-}" && -d /Applications/Xcode.app/Contents/Developer ]]; then
  export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
fi
if [[ -n "${DEVELOPER_DIR:-}" ]]; then
  export PATH="$DEVELOPER_DIR/usr/bin:$PATH"
fi
# Never inherit a desktop SDK selection when cross-compiling for iOS.
unset SDKROOT MACOSX_DEPLOYMENT_TARGET

if [[ "$PROFILE" == "release" ]]; then
  PROFILE_DIR="release"
  PROFILE_FLAG="--release"
elif [[ "$PROFILE" == "debug" ]]; then
  PROFILE_DIR="debug"
  PROFILE_FLAG="--profile=dev"
else
  PROFILE_DIR="$PROFILE"
  PROFILE_FLAG="--profile=$PROFILE"
fi

log() { printf '==> %s\n' "$*" >&2; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

need_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}
need_command cargo
need_command xcodebuild
need_command lipo

if ! xcodebuild -version >/dev/null 2>&1; then
  fail "xcodebuild is unavailable; set DEVELOPER_DIR to a full Xcode installation"
fi
if ! xcrun --sdk iphoneos --show-sdk-path >/dev/null 2>&1; then
  fail "the iOS SDK is unavailable; set DEVELOPER_DIR to a full Xcode installation"
fi

log "Xcode: $(xcodebuild -version | head -n 1)"
log "iOS SDK: $(xcrun --sdk iphoneos --show-sdk-version)"
log "Profile: $PROFILE ($PROFILE_DIR)"

# Fail early with an actionable message when a Rust target is missing.
BUILD_TARGETS=($SIM_TARGETS)
if [[ "$SIMULATOR_ONLY" != 1 ]]; then
  BUILD_TARGETS+=("$DEVICE_TARGET")
fi
for target in "${BUILD_TARGETS[@]}"; do
  if ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
    fail "Rust target $target is not installed (run: rustup target add $target)"
  fi
done

rm -rf "$BUILD_ROOT"
mkdir -p "$BUILD_ROOT" "$BINDINGS_DIR"

# --- 1. Build the host library and generate Swift bindings -------------------
log "Building host library for bindings generation"
(cd "$ROOT" && cargo build -p whalerein-mobile --lib --no-default-features "$PROFILE_FLAG" --features cli)
HOST_LIB="$CARGO_TARGET_DIR/$PROFILE_DIR/libwhalerein_mobile.dylib"
[[ -f "$HOST_LIB" ]] || fail "host library not found: $HOST_LIB"

log "Generating Swift bindings"
(cd "$ROOT" && cargo run -q -p whalerein-mobile --features cli --no-default-features "$PROFILE_FLAG" \
  --bin uniffi-bindgen -- generate --library "$HOST_LIB" --language swift --out-dir "$BINDINGS_DIR")
for artifact in WhaleReinMobile.swift WhaleReinMobileFFI.h WhaleReinMobileFFI.modulemap; do
  [[ -f "$BINDINGS_DIR/$artifact" ]] || fail "bindgen did not produce $artifact"
done

# The exact revision embedded in the library, reported by the library itself.
REVISION=$(cd "$ROOT" && cargo run -q -p whalerein-mobile --no-default-features "$PROFILE_FLAG" --bin revision)
[[ -n "$REVISION" ]] || fail "revision binary returned an empty revision"
log "Revision: $REVISION"

# --- 2. Build platform static libraries -------------------------------------
build_slice() {
  local target=$1
  log "Building $target"
  (cd "$ROOT" && IPHONEOS_DEPLOYMENT_TARGET="$MIN_IOS_VERSION" \
    cargo build -p whalerein-mobile --lib --no-default-features "$PROFILE_FLAG" --target "$target")
  local lib="$CARGO_TARGET_DIR/$target/$PROFILE_DIR/libwhalerein_mobile.a"
  [[ -f "$lib" ]] || fail "static library not found for $target: $lib"
  printf '%s\n' "$lib"
}

# Assemble `WhaleReinMobileFFI.framework` with a static binary, C header and a
# framework modulemap, so Swift can `import WhaleReinMobileFFI`.
make_framework() {
  local label=$1
  local archive=$2
  local platform=$3
  local framework_dir="$BUILD_ROOT/$label/$FRAMEWORK_NAME.framework"
  mkdir -p "$framework_dir/Headers" "$framework_dir/Modules"
  cp "$archive" "$framework_dir/$FRAMEWORK_NAME"
  cp "$BINDINGS_DIR/WhaleReinMobileFFI.h" "$framework_dir/Headers/"
  cat > "$framework_dir/Modules/module.modulemap" <<MODULEMAP
framework module $FRAMEWORK_NAME {
    header "WhaleReinMobileFFI.h"
    export *
    use "Darwin"
    use "_Builtin_stdbool"
    use "_Builtin_stdint"
}
MODULEMAP
  local supported_platform="iPhoneOS"
  local platform_name="iphoneos"
  if [[ "$platform" == "simulator" ]]; then
    supported_platform="iPhoneSimulator"
    platform_name="iphonesimulator"
  fi
  cat > "$framework_dir/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>$FRAMEWORK_NAME</string>
  <key>CFBundleIdentifier</key><string>com.whalerein.mobile.$FRAMEWORK_NAME</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>$FRAMEWORK_NAME</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>MinimumOSVersion</key><string>$MIN_IOS_VERSION</string>
  <key>CFBundleSupportedPlatforms</key><array><string>$supported_platform</string></array>
  <key>DTPlatformName</key><string>$platform_name</string>
</dict>
</plist>
PLIST
  printf '%s\n' "$framework_dir"
}

FRAMEWORKS=()
if [[ "$SIMULATOR_ONLY" != 1 ]]; then
  FRAMEWORKS+=("$(make_framework "device" "$(build_slice "$DEVICE_TARGET")" "device")")
fi

# xcodebuild requires one framework per platform. Combine multiple Simulator
# architectures into a single universal static archive first.
SIM_ARCHIVES=()
for target in $SIM_TARGETS; do
  SIM_ARCHIVES+=("$(build_slice "$target")")
done
if [[ ${#SIM_ARCHIVES[@]} -eq 1 ]]; then
  SIM_ARCHIVE=${SIM_ARCHIVES[0]}
else
  SIM_ARCHIVE="$BUILD_ROOT/libwhalerein_mobile_simulator.a"
  log "Creating universal Simulator archive: ${SIM_TARGETS// / }"
  lipo -create "${SIM_ARCHIVES[@]}" -output "$SIM_ARCHIVE"
fi
SIM_FRAMEWORK=$(make_framework "simulator" "$SIM_ARCHIVE" "simulator")
FRAMEWORKS+=("$SIM_FRAMEWORK")

# --- 3. Create the XCFramework ----------------------------------------------
log "Creating $(basename "$XCFRAMEWORK")"
mkdir -p "$VENDOR_DIR"
rm -rf "$XCFRAMEWORK"
CREATE_ARGS=()
for framework in "${FRAMEWORKS[@]}"; do
  CREATE_ARGS+=(-framework "$framework")
done
xcodebuild -create-xcframework "${CREATE_ARGS[@]}" -output "$XCFRAMEWORK"
[[ -d "$XCFRAMEWORK" ]] || fail "xcodebuild did not create $XCFRAMEWORK"

# --- 4. Install Swift sources and provenance ---------------------------------
log "Installing Swift bindings into $SWIFT_VENDOR_DIR"
mkdir -p "$SWIFT_VENDOR_DIR"
rm -f "$SWIFT_VENDOR_DIR/WhaleReinMobile.swift" \
      "$SWIFT_VENDOR_DIR/WhaleReinMobileError.swift" \
      "$SWIFT_VENDOR_DIR/WhaleReinMobile.revision"
cp "$BINDINGS_DIR/WhaleReinMobile.swift" "$SWIFT_VENDOR_DIR/WhaleReinMobile.swift"
cp "$CRATE_DIR/swift/WhaleReinMobileError.swift" "$SWIFT_VENDOR_DIR/WhaleReinMobileError.swift"
printf '%s\n' "$REVISION" > "$SWIFT_VENDOR_DIR/WhaleReinMobile.revision"
printf '%s\n' "$REVISION" > "$VENDOR_DIR/revision.txt"

# --- 5. Report ---------------------------------------------------------------
printf 'Slices:\n'
for slice in "$XCFRAMEWORK"/*; do
  [[ -d "$slice" ]] || continue
  binary="$slice/$FRAMEWORK_NAME.framework/$FRAMEWORK_NAME"
  [[ -f "$binary" ]] || fail "missing framework binary in slice: $binary"
  printf '    %s: %s\n' "$(basename "$slice")" "$(lipo -archs "$binary")"
done
if ! cmp -s "$SWIFT_VENDOR_DIR/WhaleReinMobile.revision" "$VENDOR_DIR/revision.txt"; then
  fail "revision resource mismatch"
fi

printf 'Swift module name: WhaleReinMobile (C module: WhaleReinMobileFFI)\n'
printf 'XCFramework: %s\n' "$XCFRAMEWORK"
printf 'Swift sources: %s\n' "$SWIFT_VENDOR_DIR"
printf 'Revision: %s\n' "$REVISION"
