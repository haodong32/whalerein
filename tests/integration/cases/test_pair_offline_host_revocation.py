"""Revocation reaches a Host that was disconnected when the grant was revoked."""
import select
import socket
import socketserver
import subprocess
import threading

import pytest
from environment import http, wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def scalar(schema, statement):
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
            f"SET search_path TO {schema}; {statement}",
        ],
        check=True,
        capture_output=True,
        text=True,
        timeout=5,
    )
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return lines[-1] if lines else ""


def pending_replays(schema, device_id):
    return scalar(
        schema,
        f"SELECT count(*) FROM grants WHERE device_id='{device_id}' "
        "AND revoked_at IS NOT NULL AND replayed_at IS NULL",
    )


def live_grants(schema, device_id):
    return scalar(
        schema,
        f"SELECT count(*) FROM grants WHERE device_id='{device_id}' AND revoked_at IS NULL",
    )


def revoked_token_is_rejected(token):
    with pytest.raises(RuntimeError, match="HTTP (401|403)"):
        http("/v1/hosts", token=token)


class HostMqttRelay:
    """Loopback TCP relay for one Host's MQTT connection."""

    def __init__(self):
        self._lock = threading.Lock()
        self._online = True
        self._connections = set()
        relay = self

        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                relay._forward(self.request)

        self._server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
        self.port = self._server.server_address[1]
        self._thread = threading.Thread(target=self._server.serve_forever)
        self._thread.start()

    def set_online(self, online):
        with self._lock:
            self._online = online
            if not online:
                for pair in self._connections:
                    for endpoint in pair:
                        try:
                            endpoint.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass

    def _forward(self, client):
        client.settimeout(3)
        with self._lock:
            if not self._online:
                return
        try:
            with socket.create_connection(("127.0.0.1", 1883), timeout=3) as upstream:
                pair = (client, upstream)
                with self._lock:
                    if not self._online:
                        return
                    self._connections.add(pair)
                try:
                    while True:
                        readable, _, _ = select.select(pair, [], [])
                        for endpoint in readable:
                            chunk = endpoint.recv(65536)
                            if not chunk:
                                return
                            destination = upstream if endpoint is client else client
                            destination.sendall(chunk)
                finally:
                    with self._lock:
                        self._connections.remove(pair)
        except OSError:
            # Socket closure is the fault this relay is testing.
            return

    def close(self):
        self.set_online(False)
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)
        assert not self._thread.is_alive(), "Host MQTT relay did not stop"


@pytest.fixture
def host_mqtt_relay():
    relay = HostMqttRelay()
    try:
        yield relay
    finally:
        relay.close()


@pytest.mark.description(
    "A device revocation committed while the Host is offline replays on reconnect, "
    "clears the Host peer and leaves the old token rejected"
)
def test_device_revocation_while_host_offline_replays_on_reconnect(
    scenario, paired_device, host_mqtt_relay
):
    device = paired_device
    device_id = device.identity["device_id"]
    with scenario.fixture_bearer(device) as token:
        assert scenario.request("ready")["connected"]
        assert scenario.host_peer_exists(device_id)

        native = scenario.native
        assert live_grants(scenario.schema, device_id) == "1"
        host_mqtt_relay.set_online(False)
        wait(lambda: not scenario.request("ready")["connected"], timeout=20)
        assert scenario.native is native and native.child.poll() is None
        assert http("/readyz")
        wait(
            lambda: any(
                row["host_id"] == scenario.host and not row["online"]
                for row in device.hosts()
            ),
            timeout=30,
        )
        device.revoke()
        assert live_grants(scenario.schema, device_id) == "0"
        assert pending_replays(scenario.schema, device_id) == "1"
        assert scenario.request("ready")["connected"] is False

        assert http("/readyz")
        host_mqtt_relay.set_online(True)
        wait(lambda: scenario.request("ready")["connected"], timeout=60)
        wait(
            lambda: pending_replays(scenario.schema, device_id) == "0",
            timeout=60,
        )
        wait(lambda: not scenario.host_peer_exists(device_id), timeout=30)
        wait(lambda: scenario.request("ready")["replayPending"] is False, timeout=30)
        assert scenario.native is native and native.child.poll() is None
        assert live_grants(scenario.schema, device_id) == "0"
        revoked_token_is_rejected(token)


@pytest.mark.description(
    "A Host-signed revocation submitted while the Host has no broker session is "
    "accepted, queued and replayed when the Host reconnects"
)
def test_host_signed_revocation_while_disconnected_is_queued(
    scenario, paired_device, host_mqtt_relay
):
    device = paired_device
    device_id = device.identity["device_id"]
    with scenario.fixture_bearer(device) as token:
        assert scenario.request("ready")["connected"]
        assert scenario.host_peer_exists(device_id)

        native = scenario.native
        assert live_grants(scenario.schema, device_id) == "1"
        host_mqtt_relay.set_online(False)
        wait(lambda: not scenario.request("ready")["connected"], timeout=20)
        assert scenario.native is native and native.child.poll() is None
        assert http("/readyz")
        wait(
            lambda: any(
                row["host_id"] == scenario.host and not row["online"]
                for row in device.hosts()
            ),
            timeout=30,
        )
        assert scenario.request("gx-revoke-host-grant", deviceId=device_id)["revoked"]
        assert live_grants(scenario.schema, device_id) == "0"
        assert pending_replays(scenario.schema, device_id) == "1"

        assert http("/readyz")
        host_mqtt_relay.set_online(True)
        wait(lambda: scenario.request("ready")["connected"], timeout=60)
        wait(
            lambda: pending_replays(scenario.schema, device_id) == "0",
            timeout=60,
        )
        wait(lambda: not scenario.host_peer_exists(device_id), timeout=30)
        wait(lambda: scenario.request("ready")["replayPending"] is False, timeout=30)
        assert scenario.native is native and native.child.poll() is None
        assert live_grants(scenario.schema, device_id) == "0"
        revoked_token_is_rejected(token)
