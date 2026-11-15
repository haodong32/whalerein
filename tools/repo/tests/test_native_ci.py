"""Native CI setup must verify downloads and restore Keychain state on failure."""

import hashlib
import io
import json
import subprocess
from types import SimpleNamespace

import pytest

from repo import native_ci


def test_protoc_download_rejects_corruption_before_installation(monkeypatch, tmp_path):
    monkeypatch.setattr(
        native_ci.urllib.request, "urlopen", lambda *a, **kw: io.BytesIO(b"compiler")
    )
    assert (
        native_ci.download_verified("url", hashlib.sha256(b"compiler").hexdigest()) == b"compiler"
    )
    with pytest.raises(ValueError, match="checksum"):
        native_ci.download_verified("url", "incorrect")
    monkeypatch.setattr(native_ci, "sys", SimpleNamespace(platform="win32"))
    monkeypatch.setattr(native_ci.platform, "machine", lambda: "AMD64")
    monkeypatch.setitem(
        native_ci.PROTOC,
        ("win32", "amd64"),
        ("windows-x86_64", hashlib.sha256(b"compiler").hexdigest()),
    )
    native_ci.install_protoc(tmp_path)
    assert (tmp_path / "protoc.exe").read_bytes() == b"compiler"


def test_native_protoc_rejects_unsupported_platform(monkeypatch, tmp_path):
    monkeypatch.setattr(native_ci, "sys", SimpleNamespace(platform="unsupported"))
    with pytest.raises(ValueError, match="Unsupported"):
        native_ci.install_protoc(tmp_path)


@pytest.mark.parametrize("failure", ["nonzero", "missing", "timeout"])
def test_native_command_failures_never_export_arguments_or_output(monkeypatch, failure):
    def run(*args, **kwargs):
        if failure == "missing":
            raise OSError("synthetic-private")
        if failure == "timeout":
            raise subprocess.TimeoutExpired(["synthetic-private"], 1)
        return SimpleNamespace(returncode=1, stdout="synthetic-private", stderr="synthetic-private")

    monkeypatch.setattr(native_ci.subprocess, "run", run)
    with pytest.raises(RuntimeError) as error:
        native_ci.call(["synthetic-private"])
    assert "synthetic-private" not in str(error.value)


@pytest.mark.parametrize("outcome", ["passed", "failed", "not executed", "cleanup failed"])
def test_keychain_restores_default_and_search_list_even_after_test_failure(
    monkeypatch, tmp_path, outcome
):
    calls = []
    monkeypatch.setattr(native_ci, "sys", SimpleNamespace(platform="darwin"))

    def run(command, **kwargs):
        calls.append(command)
        if command[:2] == ["security", "default-keychain"] and "-s" not in command:
            return SimpleNamespace(returncode=0, stdout='"/original/login.keychain-db"', stderr="")
        if command[:2] == ["security", "list-keychains"] and "-s" not in command:
            return SimpleNamespace(
                returncode=0, stdout='"/original/login.keychain-db" "another keychain"', stderr=""
            )
        if command[0] == "cargo":
            return SimpleNamespace(
                returncode=int(outcome == "failed"),
                stdout=(
                    "test result: ok. 0 passed; 0 failed; 0 ignored;"
                    if outcome == "not executed"
                    else "test result: ok. 1 passed; 0 failed; 0 ignored;"
                ),
                stderr="synthetic-private",
            )
        return SimpleNamespace(
            returncode=int(outcome == "cleanup failed" and command[1] == "delete-keychain"),
            stdout="",
            stderr="",
        )

    monkeypatch.setattr(native_ci.subprocess, "run", run)
    output = tmp_path / "result.json"
    if outcome == "passed":
        native_ci.verify_keychain(output)
        assert json.loads(output.read_text())["executed"] == 1
    else:
        with pytest.raises(RuntimeError):
            native_ci.verify_keychain(output)
        assert not output.exists()
    assert calls[-3] == [
        "security",
        "default-keychain",
        "-d",
        "user",
        "-s",
        "/original/login.keychain-db",
    ]
    assert calls[-2] == [
        "security",
        "list-keychains",
        "-d",
        "user",
        "-s",
        "/original/login.keychain-db",
        "another keychain",
    ]
    assert calls[-1][1] == "delete-keychain"
    command = next(c for c in calls if c[0] == "cargo")
    assert "--ignored" in command and "--exact" in command


def test_keychain_refuses_other_operating_systems(monkeypatch, tmp_path):
    monkeypatch.setattr(native_ci, "sys", SimpleNamespace(platform="win32"))
    with pytest.raises(ValueError, match="native macOS"):
        native_ci.verify_keychain(tmp_path / "result.json")
