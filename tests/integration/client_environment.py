"""Production CLI clients with a private test-run Secret Service."""

import json
import os
import re
import subprocess
import uuid
from contextlib import contextmanager

from environment import ROOT, Process, Watcher, wait


@contextmanager
def isolated_keyring(tmp_path):
    """Run a fresh Linux Secret Service on a private session bus."""
    directory = tmp_path / "platform-keyring"
    directory.mkdir(mode=0o700)
    data = directory / "data"
    data.mkdir(mode=0o700)
    # Keep the UNIX socket path short even when pytest uses a long case name.
    address = f"unix:abstract=whalerein-{uuid.uuid4().hex}"
    env = {
        "DBUS_SESSION_BUS_ADDRESS": address,
        "XDG_RUNTIME_DIR": str(directory),
        "XDG_DATA_HOME": str(data),
    }
    bus = Process(
        ["dbus-daemon", "--session", "--nofork", "--address", address],
        directory,
        env,
    )
    keyring = None
    daemon_probe = [
        "dbus-send",
        "--session",
        "--print-reply",
        "--reply-timeout=250",
    ]

    def reply(destination, path, method, *args):
        result = subprocess.run(
            [*daemon_probe, f"--dest={destination}", path, method, *args],
            env={**os.environ, **env},
            capture_output=True,
            timeout=2,
            check=False,
        )
        return result.stdout if result.returncode == 0 else b""

    def unlocked_default_collection():
        # A call to Secret Service before our daemon owns the name would ask
        # D-Bus to auto-start a second daemon without the unlock input.
        owner = reply(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus.NameHasOwner",
            "string:org.freedesktop.secrets",
        )
        if b"boolean true" not in owner:
            return False
        alias = reply(
            "org.freedesktop.secrets",
            "/org/freedesktop/secrets",
            "org.freedesktop.Secret.Service.ReadAlias",
            "string:default",
        )
        collection = b'/org/freedesktop/secrets/collection/'
        if collection not in alias:
            return False
        path = alias.split(collection, 1)[1].split(b'"', 1)[0]
        return b"boolean false" in reply(
            "org.freedesktop.secrets",
            (collection + path).decode(),
            "org.freedesktop.DBus.Properties.Get",
            "string:org.freedesktop.Secret.Collection",
            "string:Locked",
        )

    try:
        wait(
            lambda: reply(
                "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus.ListNames"
            ),
            timeout=10,
            process=bus,
        )
        keyring = Process(
            ["gnome-keyring-daemon", "--foreground", "--unlock", "--components=secrets"],
            directory,
            env,
            control=True,
        )
        keyring.child.stdin.write(uuid.uuid4().hex)
        keyring.child.stdin.close()
        wait(unlocked_default_collection, timeout=30, process=keyring)
        yield env
    finally:
        if keyring is not None:
            keyring.close()
        bus.close()


def fixture_secret_username(device, key):
    """Resolve a supported fixture slot using only the instance identity."""
    if key not in {"device-identity", "device-token", "cache-key", "pending-pairing"}:
        raise ValueError("Unknown fixture secret role")
    manifest_path = device.instance_dir / "instance.json"
    if not manifest_path.is_file():
        return None
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    instance_id = manifest["id"]
    if not re.fullmatch(r"[a-zA-Z0-9_-]+", instance_id):
        raise AssertionError("Unexpected fixture instance identity")
    if key != "device-identity":
        device_id = device.identity["device_id"]
        if not re.fullmatch(r"[a-zA-Z0-9_-]+", device_id):
            raise AssertionError("Unexpected fixture Device identity")
        key = f"whalerein:v1:{key}:{device_id}"
    return f"whalerein:instance:{instance_id}:{key}"


def _fixture_secret_environment(device):
    environment = device.keyring_env()
    address = environment.get("DBUS_SESSION_BUS_ADDRESS", "")
    if not re.fullmatch(r"unix:abstract=whalerein-[0-9a-f]{32}", address):
        raise AssertionError("Refusing to access a non-fixture Secret Service")
    return environment


def fixture_has_secret(device, key):
    """Check only Secret Service item metadata without reading any value."""
    username = fixture_secret_username(device, key)
    if username is None:
        return False
    result = subprocess.run(
        [
            "dbus-send", "--session", "--print-reply", "--reply-timeout=1000",
            "--dest=org.freedesktop.secrets", "/org/freedesktop/secrets",
            "org.freedesktop.Secret.Service.SearchItems",
            f"dict:string:string:service,whalerein,username,{username}",
        ],
        env=_fixture_secret_environment(device),
        capture_output=True,
        timeout=5,
        check=False,
    )
    assert result.returncode == 0, "Fixture Secret Service item search failed"
    return b'object path "/org/freedesktop/secrets/collection/' in result.stdout


def remove_fixture_cache_key(device):
    """Delete only the case-owned cache key via Secret Service attributes."""
    username = fixture_secret_username(device, "cache-key")
    assert username is not None, "Fixture cache key removal requires an instance"
    result = subprocess.run(
        ["secret-tool", "clear", "service", "whalerein", "username", username],
        env=_fixture_secret_environment(device), capture_output=True, timeout=5, check=False,
    )
    assert result.returncode == 0, "Fixture cache key removal failed"


class CutoverClient:
    def __init__(self, directory, instance, keyring):
        self.config = directory / f"{instance}.toml"
        self.config.write_text(
            f'version = 1\nservice = "standalone"\ndata_dir = "{instance}"\n'
        )
        self.env = {**os.environ, **keyring}
        self.env.pop("WHALEREIN_CONFIG", None)

    def args(self, *commands):
        return [
            str(ROOT / "target/debug/whalerein"),
            "--config",
            str(self.config),
            "--json",
            *commands,
        ]

    def watch(self, *commands):
        """Read live NDJSON from the exact selected-file CLI command."""
        return Watcher(self.args(*commands), self.env)

    def invoke(self, *commands, input_text=None):
        try:
            process = subprocess.run(
                self.args(*commands),
                cwd=ROOT,
                env=self.env,
                input=input_text,
                capture_output=True,
                text=True,
                timeout=45,
                check=False,
            )
        except subprocess.TimeoutExpired as error:
            # Keep the failing command family in the recorded reason; the
            # session identifier and stdin payload stay out of it.
            family = " ".join(str(command) for command in commands[:2])
            raise TimeoutError(f"CLI {family} timed out after 45s") from error
        return process.returncode, self.envelope(process.stdout)

    def call(self, *commands, input_text=None):
        exit_code, result = self.invoke(*commands, input_text=input_text)
        if exit_code != 0:
            code = result.get("error", {}).get("code", "unknown")
            raise AssertionError(f"CLI {commands[:2]} failed with {code}: {result}")
        return result["data"]

    @staticmethod
    def envelope(output):
        lines = output.splitlines()
        assert len(lines) == 1, "Finite command did not return one JSON envelope"
        result = json.loads(lines[0])
        assert result.get("schema") == "whalerein/client/v1"
        return result
