"""Interactive fingerprint refusal and Ctrl-C never reach FINISH."""
import os
import pty
import select
import signal
import subprocess
import termios
import time

import pytest
from environment import ROOT, Device, http
from journey_helpers import sql as scalar

pytestmark = pytest.mark.chain


def interactive_pair(device, uri, action, timeout=30):
    """Run `pair` on a pseudo-terminal and act once the fingerprint prompt appears.

    `action` is `sigint`, `eof`, or bytes to write (for example `b"n\\n"`).
    Returns the process exit code after asserting the prompt was actually shown.
    """
    master, slave = pty.openpty()
    settings = termios.tcgetattr(slave)
    settings[3] &= ~termios.ECHO
    termios.tcsetattr(slave, termios.TCSANOW, settings)
    process = subprocess.Popen(
        device.args("host", "connect", "--invite-stdin"),
        cwd=ROOT,
        env=device.env(),
        stdin=slave,
        stdout=slave,
        stderr=slave,
        start_new_session=True,
    )
    os.close(slave)
    os.write(master, uri.encode() + b"\n\x04")
    output = bytearray()
    try:
        deadline = time.monotonic() + timeout
        while b"[y/N]" not in output and time.monotonic() < deadline:
            if process.poll() is not None:
                break
            ready, _, _ = select.select([master], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(master, 4096)
            except OSError:
                break
            if not chunk:
                break
            output.extend(chunk)
        assert b"[y/N]" in output, (
            "pair did not show the fingerprint prompt: "
            + output[-400:].decode("utf-8", "replace")
        )
        if action == "sigint":
            os.killpg(process.pid, signal.SIGINT)
        elif action == "eof":
            # Ctrl-D makes read_line return EOF without closing the terminal,
            # so the CLI can still report its cancellation envelope.
            os.write(master, b"\x04")
        else:
            os.write(master, action)
        try:
            return process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
            raise AssertionError("interactive pair did not exit after the prompt")
    finally:
        if master is not None:
            try:
                os.close(master)
            except OSError:
                pass
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)


def fields(scenario):
    grants = scalar(scenario.schema, "SELECT count(*) FROM grants")
    finishes = scalar(
        scenario.schema,
        "SELECT count(*) FROM pairing_operations WHERE finish_hash IS NOT NULL",
    )
    return grants, finishes


@pytest.mark.description(
    "Ctrl-C at the fingerprint prompt sends no FINISH and a retry creates exactly one grant"
)
def test_fingerprint_prompt_ctrl_c_then_retry_creates_one_grant(scenario):
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )
    device = Device(scenario.directory, "client")
    code = interactive_pair(device, invitation["pairingUri"], "sigint")
    assert code != 0
    assert fields(scenario) == ("0", "0"), "Ctrl-C sent a FINISH or created a grant"
    assert not (device.data_dir / "regional.json").exists()

    device.identity = device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation["pairingUri"])
    assert fields(scenario) == ("1", "1")
    assert scenario.host in {host["host_id"] for host in device.hosts()}
    assert (
        scalar(
            scenario.schema,
            f"SELECT count(*) FROM grants WHERE device_id='{device.identity['device_id']}'",
        )
        == "1"
    )


@pytest.mark.description(
    "n, an empty line and EOF at the fingerprint prompt each refuse without FINISH "
    "and the same invitation still pairs once"
)
def test_fingerprint_prompt_refusals_send_no_finish(scenario):
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )
    device = Device(scenario.directory, "client")
    for action in (b"n\n", b"\n", "eof"):
        code = interactive_pair(device, invitation["pairingUri"], action)
        assert code == 2, f"refusal {action!r} must exit with the cancellation code"
        assert fields(scenario) == ("0", "0"), (
            f"refusal {action!r} sent a FINISH or created a grant"
        )

    device.identity = device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation["pairingUri"])
    assert fields(scenario) == ("1", "1")
    assert device.sessions(scenario.host) == []
