"""Pinned native CI prerequisites and isolated macOS SecretStore verification."""

import argparse
import hashlib
import json
import platform
import re
import secrets
import shlex
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

PROTOC = {
    ("darwin", "arm64"): (
        "osx-aarch_64",
        "765ccb18be73e565bfc3e2ad94e85039ca28f928b3e62b487e51b7b8fdf1dcfb",
    ),
    ("darwin", "x86_64"): (
        "osx-x86_64",
        "56ba0f583d8761975d177670a3375564fd2bf5a7d96182a9d0c3b818a893c0ff",
    ),
    ("win32", "amd64"): (
        "windows-x86_64",
        "5a1b5350308309c9729ce4484a798981f281c10909144c4bdefd6a37688e4b1f",
    ),
}
KEYCHAIN_CASE = (
    "security::secrets::keyring_manual::stores_loads_and_deletes_binary_secrets_in_macos_keychain"
)


def download_verified(url, checksum):
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    if hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError("Native protoc checksum mismatch")
    return data


def install_protoc(directory):
    target = PROTOC.get((sys.platform, platform.machine().lower()))
    if target is None:
        raise ValueError("Unsupported native protoc platform")
    classifier, checksum = target
    data = download_verified(
        f"https://repo.maven.apache.org/maven2/com/google/protobuf/protoc/4.36.2/protoc-4.36.2-{classifier}.exe",
        checksum,
    )
    directory.mkdir(parents=True, exist_ok=True)
    executable = directory / ("protoc.exe" if sys.platform == "win32" else "protoc")
    executable.write_bytes(data)
    executable.chmod(0o755)


def call(command, timeout=60):
    # Never include commands or captured output in exceptions: security arguments
    # contain the temporary keychain password, and libtest can print secret bytes.
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=timeout, check=False
        )
    except (OSError, subprocess.TimeoutExpired):
        raise RuntimeError("Native CI command could not complete") from None
    if result.returncode:
        raise RuntimeError("Native CI command failed")
    return result.stdout


def verify_keychain(output):
    if sys.platform != "darwin":
        raise ValueError("Keychain verification requires native macOS")
    default = shlex.split(call(["security", "default-keychain", "-d", "user"]))[0]
    search = shlex.split(call(["security", "list-keychains", "-d", "user"]))
    with tempfile.TemporaryDirectory(prefix="whalerein-keychain-") as directory:
        keychain = str(Path(directory) / "test.keychain-db")
        password = secrets.token_hex(32)
        call(["security", "create-keychain", "-p", password, keychain])
        try:
            call(["security", "set-keychain-settings", "-lut", "7200", keychain])
            call(["security", "unlock-keychain", "-p", password, keychain])
            call(["security", "list-keychains", "-d", "user", "-s", keychain, *search])
            call(["security", "default-keychain", "-d", "user", "-s", keychain])
            result = call(
                [
                    "cargo",
                    "test",
                    "--package",
                    "whalerein-core",
                    "--lib",
                    KEYCHAIN_CASE,
                    "--",
                    "--ignored",
                    "--exact",
                    "--test-threads=1",
                ],
                timeout=1800,
            )
            if not re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", result):
                raise RuntimeError("Keychain test did not execute exactly once")
        finally:
            failed = False
            for command in (
                ["security", "default-keychain", "-d", "user", "-s", default],
                ["security", "list-keychains", "-d", "user", "-s", *search],
                ["security", "delete-keychain", keychain],
            ):
                try:
                    call(command)
                except RuntimeError:
                    failed = True
            if failed:
                raise RuntimeError("Keychain cleanup failed")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps({"test": KEYCHAIN_CASE, "status": "passed", "executed": 1}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("protoc", "keychain"))
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    if args.operation == "protoc":
        install_protoc(args.destination)
    else:
        verify_keychain(args.destination)


if __name__ == "__main__":
    main()
