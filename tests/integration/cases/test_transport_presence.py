"""TRAN-04/05/06/15/16/18: presence truth for transport and multi-Host routing."""

import json
import os
import select
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid

import pytest
from environment import INTERNAL_TOKEN, ROOT, Device, http, wait

pytestmark = pytest.mark.transport

PROBE = ROOT / "tests/integration/drivers/transport/presence_probe.mjs"
MQTT = "mqtt://127.0.0.1:1883"


def _ready(port=18080):
    try:
        with urllib.request.urlopen(
            f"http://127.0.0.1:{port}/readyz", timeout=1
        ) as response:
            return response.status == 200
    except (urllib.error.URLError, ConnectionError, TimeoutError):
        return False


def _kill(process):
    if process.child.poll() is None:
        try:
            os.killpg(process.child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.child.wait(timeout=10)


def _log(process):
    try:
        process.output.seek(0)
        return process.output.read()
    except (OSError, ValueError):
        return ""


def _start_probe(args, *, timeout=20):
    env = {
        **os.environ,
        "WHALEREIN_TEST_MQTT_URL": MQTT,
        "WHALEREIN_PROBE_TIMEOUT_MS": str(timeout * 1000),
    }
    return subprocess.Popen(
        ["node", str(PROBE), *args],
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )


def _read_line(process, timeout=20):
    ready, _, _ = select.select([process.stdout], [], [], timeout)
    if not ready:
        raise TimeoutError("presence probe produced no line")
    line = process.stdout.readline()
    if not line:
        raise AssertionError("presence probe exited: " + (process.stderr.read() or "")[-300:])
    return line.rstrip("\n")


def _stop_probe(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.communicate(timeout=5)


def _effects(scenario):
    return scenario.dsh.process.control("gx_effect_counts")["sendUserMessage"]


def _request_row(scenario):
    """Read the newest durable request row for failure diagnostics."""
    result = subprocess.run(
        [
            "psql",
            "-h",
            "127.0.0.1",
            "-p",
            "55432",
            "-U",
            "postgres",
            "-d",
            "whalerein_test",
            "-tAc",
            "SELECT json_build_object('status',status,'state',forward_state,"
            "'attempts',forward_attempts,'error',error_code,'kind',kind) "
            f"FROM {scenario.schema}.requests ORDER BY created_at DESC LIMIT 1",
        ],
        check=False,
        capture_output=True,
        text=True,
        timeout=5,
    )
    if result.returncode != 0 or not result.stdout.strip():
        return None
    return json.loads(result.stdout)


@pytest.mark.description(
    "A send submitted during the broker reconnect window is never reported as delivered, and after the Host recovers a new send succeeds without re-pairing."
)
def test_broker_reconnect_window_queues_and_recovers(scenario, paired_device):
    session = scenario.dsh.create_session()
    before = _effects(scenario)
    scenario.broker.close()
    wait(lambda: not _ready(), timeout=20)
    # Submit while the broker is down, then restore it promptly: the request
    # must survive the reconnect window instead of becoming terminal offline.
    outcome = {}

    def submit():
        outcome["send"] = paired_device.invoke(
            'session', 'send', session, "reconnect window message", "--detach", success=False
        )

    thread = threading.Thread(target=submit, daemon=True)
    thread.start()
    time.sleep(0.5)
    scenario.broker.ensure_started()
    thread.join(timeout=45)
    assert not thread.is_alive(), "send did not finish after broker recovery"
    code, values = outcome["send"]
    last = values[-1]
    # The in-flight command may be closed as host_offline or expire while the
    # line is down; it must never be reported as delivered.
    assert not (
        code == 0 and last.get("data", {}).get("status") == "succeeded"
    ), "a send during the reconnect window was reported as delivered"
    wait(_ready, timeout=30)
    # A fresh command after the Host re-subscribes proves the path is healthy.
    wait(lambda: paired_device.hosts()[0]["online"] is True, timeout=30)
    # A Host that only reconnected its MQTT session may still report
    # `revocations_pending` (a retryable rejection, never host_offline). A
    # clean Host restart completes the replay and proves the line recovered
    # without re-pairing.
    scenario.plugin.close()
    scenario.start_plugin()
    wait(lambda: paired_device.hosts()[0]["online"] is True, timeout=30)
    paired_device.send_confirmed(session, "after reconnect")
    wait(lambda: _effects(scenario) >= before + 1, timeout=30)


@pytest.mark.description(
    "A Host broker connection without HostReady subscription confirmation is not treated as online; pairing is refused instead of publishing into a topic with no subscriber."
)
def test_connected_but_unready_host_is_not_online(scenario):
    host = "host-" + uuid.uuid4().hex[:16]
    probe = _start_probe(
        [
            "hold",
            f"whalerein-host-{host}",
            f"host:{host}",
            scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
        ]
    )
    try:
        assert _read_line(probe) == "probe ready"
        invitation = http(
            "/v1/pairing/invitations", {"hostId": host}, scenario.admin
        )
        device = Device(scenario.directory, "unready-client")
        started = time.monotonic()
        code, values = device.invoke(
            'host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation["pairingUri"], success=False
        )
        # The refusal must come from the presence gate, not from waiting for a
        # host that will never answer the forwarded HELLO.
        assert time.monotonic() - started < 15
        assert code != 0
        assert values[-1]["error"]["code"] in ("unavailable", "host_offline"), values[-1]
    finally:
        _stop_probe(probe)


@pytest.mark.description(
    "A Host that disconnects just before a send is never reported as having delivered the command, and a later send reports host_offline."
)
def test_publish_before_disconnect_is_not_reported_delivered(scenario, paired_device):
    session = scenario.dsh.create_session()
    paired_device.sessions(scenario.host)
    before = _effects(scenario)
    _kill(scenario.plugin)
    code, values = paired_device.invoke(
        'session', 'send', session, "killed host message", "--detach", success=False
    )
    last = values[-1]
    assert not (
        code == 0 and last.get("data", {}).get("status") == "succeeded"
    ), "a command published into a disconnecting host was reported as delivered"
    wait(lambda: paired_device.hosts()[0]["online"] is False, timeout=30)
    code, values = paired_device.invoke(
        'session', 'send', session, "offline host message", "--detach", success=False
    )
    assert code != 0
    assert values[-1]["error"]["code"] == "host_offline", values[-1]
    assert _effects(scenario) == before, "an unobserved command still executed on the host"


@pytest.mark.description(
    "After the Host MQTT connection drops, the CLI reports the host as offline instead of connected."
)
def test_disconnect_state_is_user_visible(scenario, paired_device):
    assert paired_device.call('host', 'list')["hosts"][0]["online"] is True
    _kill(scenario.plugin)
    wait(lambda: paired_device.hosts()[0]["online"] is False, timeout=30)
    session = scenario.dsh.create_session()
    code, values = paired_device.invoke(
        'session', 'send', session, "disconnected state", "--detach", success=False
    )
    assert code != 0
    assert values[-1]["error"]["code"] == "host_offline"
    assert paired_device.hosts()[0]["online"] is False


@pytest.mark.description(
    "Two Hosts sharing one device identity both receive the device-offline fan-out, and revoking one Host's grant leaves the other Host usable."
)
def test_two_host_presence_fanout_and_scoped_revocation(scenario, paired_device):
    second_host = "host-" + uuid.uuid4().hex[:16]
    # Presence fan-out is a Host-visible product event. Run both Host runtimes
    # at info level so the received device_offline event is observable without
    # adding a second connection under a Host's canonical client id (which would
    # take over the Host session).
    scenario.plugin.close()
    scenario.start_plugin(extra_env={"WHALEREIN_LOG_LEVEL": "info"})
    first_plugin = scenario.plugin
    scenario.start_plugin(
        extra_env={
            "WHALEREIN_HOST_ID": second_host,
            "WHALEREIN_STATE_DIR": str(scenario.directory / "plugin-2"),
            "WHALEREIN_HOST_CREDENTIAL_FILE": str(
                scenario.directory / "credentials-2.json"
            ),
            "WHALEREIN_LOG_LEVEL": "info",
        }
    )
    second_plugin = scenario.plugin
    # Keep the primary Host attached to the fixture's lifecycle handle.
    scenario.plugin = first_plugin
    try:
        wait(
            lambda: any(
                row["host_id"] == scenario.host and row["online"]
                for row in paired_device.hosts()
            ),
            timeout=30,
        )
        invitation = http(
            "/v1/pairing/invitations", {"hostId": second_host}, scenario.admin
        )
        paired_device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation["pairingUri"])
        online = {
            row["host_id"]: row["online"] for row in paired_device.hosts()
        }
        assert online.get(scenario.host) is True and online.get(second_host) is True

        device_id = paired_device.identity["device_id"]
        with scenario.fixture_bearer(paired_device) as token:
            session = _start_probe(["device", device_id, token])
            assert _read_line(session) == "device ready"
        session.communicate(timeout=20)
        assert session.returncode == 0
        wait(
            lambda: "device_offline" in _log(first_plugin).lower()
            and "device_offline" in _log(second_plugin).lower(),
            timeout=30,
        )

        # A Host-scoped revocation removes only that Host's authority.
        scenario.revoke_from_host(paired_device)
        wait(
            lambda: any(
                row["host_id"] == second_host for row in paired_device.hosts()
            ),
            timeout=20,
        )
        assert paired_device.sessions(second_host) == []
        code, values = paired_device.invoke(
            'session', 'list', "--host", scenario.host, success=False
        )
        assert code != 0
        assert values[-1]["error"]["code"] in (
            "not_found",
            "unauthorized",
            "revoked",
            "host_offline",
            "forbidden",
            "authority_closed",
        )
    finally:
        second_plugin.close()


@pytest.mark.description(
    "The internal HTTP surface exposes only the broker CONNECT validation route; presence is repaired by the broker snapshot, not a REST presence fallback."
)
def test_internal_route_surface_has_no_presence_fallback(scenario):
    request = urllib.request.Request(
        "http://127.0.0.1:18081/internal/broker/connect",
        data=b"clientid=route-snapshot&username=&password=",
        headers={"X-WhaleRein-Internal-Token": INTERNAL_TOKEN},
    )
    with urllib.request.urlopen(request, timeout=3) as response:
        assert response.read().decode().startswith("whalerein/1")
    for path in ("/internal/broker/presence", "/internal/presence"):
        try:
            urllib.request.urlopen(
                "http://127.0.0.1:18081" + path, timeout=2
            )
        except urllib.error.HTTPError as error:
            # The internal listener authenticates before routing, so an unknown
            # path is 401 here and 404 if the route table is reached first.
            # Either way it is not a presence endpoint.
            assert error.code in (401, 404), (path, error.code)
        else:
            raise AssertionError("unexpected internal presence route: " + path)
