"""Prepare the Rust bindings before Xcode resolves generated build inputs."""

import fcntl
import hashlib
import os
import plistlib
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
VENDOR = ROOT / "apps/ios/Vendor"
XCFRAMEWORK = VENDOR / "WhaleReinMobile.xcframework/Info.plist"
REVISION = VENDOR / "revision.txt"
STAMP = VENDOR / ".binding-inputs"
INPUTS = (
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo",
    "apps/mobile-bindings",
    "core",
    "proto",
    "apps/ios/scripts/build-catalyst-slice.sh",
)
BUILD_ENV = (
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "DEVELOPER_DIR",
    "PROFILE",
    "RUSTFLAGS",
    "RUSTUP_TOOLCHAIN",
    "SIM_TARGETS",
    "XCODE_PRODUCT_BUILD_VERSION",
)


def fingerprint(environment):
    digest = hashlib.sha256()
    for name in BUILD_ENV:
        digest.update(f"{name}={environment.get(name, '')}\0".encode())
    for relative in INPUTS:
        path = ROOT / relative
        files = (
            sorted(p for p in path.rglob("*") if p.is_file())
            if path.is_dir()
            else [path]
        )
        for file in files:
            if file.is_file():
                digest.update(os.fsencode(file.relative_to(ROOT)) + b"\0")
                digest.update(hashlib.sha256(file.read_bytes()).digest())
    return digest.hexdigest()


def ready(inputs, variant, source):
    if not all(
        path.is_file()
        for path in (
            STAMP,
            XCFRAMEWORK,
            REVISION,
            VENDOR / "WhaleReinMobile/WhaleReinMobile.swift",
            VENDOR / "WhaleReinMobile/WhaleReinMobileError.swift",
        )
    ):
        return False
    if STAMP.read_text() != inputs:
        return False
    if source and not REVISION.read_text().startswith(f"source:{source} "):
        return False
    try:
        with XCFRAMEWORK.open("rb") as info:
            libraries = plistlib.load(info)["AvailableLibraries"]
        return any(
            library["SupportedPlatform"] == "ios"
            and library.get("SupportedPlatformVariant") == variant
            and (
                XCFRAMEWORK.parent
                / library["LibraryIdentifier"]
                / library["LibraryPath"]
                / "WhaleReinMobileFFI"
            ).is_file()
            for library in libraries
        )
    except (KeyError, plistlib.InvalidFileException):
        return False


def main():
    environment = dict(os.environ)
    # Xcode exports these Swift settings to the scheme action. Rust's tools
    # warn about them, and the binding build has its own debug settings.
    environment.pop("SWIFT_DEBUG_INFORMATION_FORMAT", None)
    environment.pop("SWIFT_DEBUG_INFORMATION_VERSION", None)
    environment["PATH"] += os.pathsep + os.pathsep.join(
        (
            str(Path(environment.get("CARGO_HOME", Path.home() / ".cargo")) / "bin"),
            "/opt/homebrew/bin",
            "/usr/local/bin",
        )
    )
    platform = environment.get("EFFECTIVE_PLATFORM_NAME")
    variant = {"-iphonesimulator": "simulator", "-maccatalyst": "maccatalyst"}.get(
        platform
    )
    environment["WHALEREIN_MOBILE_SIMULATOR_ONLY"] = (
        "1" if variant == "simulator" else "0"
    )
    lock = ROOT / "target/whalerein-ios-bindings.lock"
    lock.parent.mkdir(exist_ok=True)
    with lock.open("a") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        inputs = fingerprint(environment)
        if ready(inputs, variant, environment.get("WHALEREIN_MOBILE_SOURCE")):
            return
        if STAMP.exists():
            STAMP.unlink()
        subprocess.run(
            ["bash", "apps/mobile-bindings/scripts/build-xcframework.sh"],
            cwd=ROOT,
            env=environment,
            check=True,
        )
        if variant == "maccatalyst":
            subprocess.run(
                ["bash", "apps/ios/scripts/build-catalyst-slice.sh"],
                cwd=ROOT,
                env=environment,
                check=True,
            )
        STAMP.write_text(inputs)


if __name__ == "__main__":
    main()
