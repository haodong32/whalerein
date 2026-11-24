"""Deterministic transport faults for Access forwarding recovery cases.

These helpers inject real socket-level faults. They never fake an Access
status, a Host journal entry or a source effect: the MQTT PUBLISH really
reaches the broker (and the Host), only its confirmation is lost on the way
back to Access.
"""

import socket
import threading

PUBLISH = 3
COMMAND_REQUEST_KIND = 3


def _varint(data, index):
    value = 0
    shift = 0
    while index < len(data) and shift < 28:
        byte = data[index]
        index += 1
        value |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return value, index
        shift += 7
    return None, index


def _field(payload, number):
    """Read one protobuf field by number, ignoring unknown wire types."""
    index = 0
    while index < len(payload):
        key, index = _varint(payload, index)
        if key is None:
            return None
        wire = key & 0x07
        field = key >> 3
        if wire == 0:
            value, index = _varint(payload, index)
            if value is None:
                return None
        elif wire == 2:
            length, index = _varint(payload, index)
            if length is None or index + length > len(payload):
                return None
            value = payload[index : index + length]
            index += length
        elif wire == 1:
            index += 8
        elif wire == 5:
            index += 4
        else:
            return None
        if field == number:
            return value
    return None


def _host_request_kind(payload):
    """Return the complete HostRequest.kind from one relay PUBLISH.

    Epoch 4 carries exactly one envelope per packet, so the payload is the
    protobuf message itself. There is no legacy prefix to unwrap.
    """
    kind = _field(payload, 4)
    return kind if isinstance(kind, int) else None


def _packet(buffer):
    """Split one complete MQTT packet from the front of the buffer."""
    if len(buffer) < 2:
        return None
    length, index = _varint(buffer, 1)
    if length is None:
        return None
    end = index + length
    if end > len(buffer):
        return None
    return buffer[:end], buffer[end:]


class AmbiguousHostRequestProxy:
    """MQTT proxy that loses the confirmation of one class of publishes.

    Access connects through this loopback proxy instead of the broker. When
    Access publishes a COMMAND HostRequest for the selected Host, the packet is
    written to the real broker and the connection is then closed before the
    broker acknowledgement can come back. The command may therefore already
    have executed while the publish returns an error, which is exactly the
    documented ambiguous-delivery window. There is no application receipt in
    epoch 5, so this proxy withholds the only remaining confirmation.
    """

    def __init__(self, host_id, upstream=("127.0.0.1", 1883)):
        self.topic = f"whalerein/v1/h/{host_id}/request".encode()
        self.upstream = upstream
        self._lock = threading.Lock()
        self._armed = True
        self._faults = 0

        self._server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._server.bind(("127.0.0.1", 0))
        self._server.listen(64)
        self._server.settimeout(0.2)
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    @property
    def url(self):
        return f"mqtt://127.0.0.1:{self._server.getsockname()[1]}"

    @property
    def faults(self):
        with self._lock:
            return self._faults

    def disarm(self):
        with self._lock:
            self._armed = False

    def close(self):
        self.disarm()
        try:
            self._server.close()
        except OSError:
            pass
        self._thread.join(timeout=5)

    def _serve(self):
        while True:
            try:
                client, _ = self._server.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            threading.Thread(target=self._handle, args=(client,), daemon=True).start()

    def _handle(self, client):
        try:
            upstream = socket.create_connection(self.upstream, timeout=10)
        except OSError:
            client.close()
            return
        state = {"faulted": False}
        relay = threading.Lock()

        def pump_upstream():
            while True:
                try:
                    chunk = upstream.recv(65536)
                except OSError:
                    return
                if not chunk:
                    return
                # Holding the relay lock while checking the fault gates the
                # broker acknowledgement of the command publish: once the
                # fault is armed no later upstream byte reaches Access.
                with relay:
                    if state["faulted"]:
                        return
                    try:
                        client.sendall(chunk)
                    except OSError:
                        return

        reader = threading.Thread(target=pump_upstream, daemon=True)
        reader.start()
        try:
            self._pump_client(client, upstream, state, relay)
        finally:
            for sock in (client, upstream):
                try:
                    sock.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                try:
                    sock.close()
                except OSError:
                    pass
            reader.join(timeout=5)

    def _pump_client(self, client, upstream, state, relay):
        buffer = b""
        while not state["faulted"]:
            try:
                chunk = client.recv(65536)
            except OSError:
                return
            if not chunk:
                return
            buffer += chunk
            while True:
                parsed = _packet(buffer)
                if parsed is None:
                    break
                packet, buffer = parsed
                if self._is_command_publish(packet):
                    # Mark the fault before the packet reaches the broker so
                    # the acknowledgement cannot be relayed to Access.
                    with relay:
                        state["faulted"] = True
                    try:
                        upstream.sendall(packet)
                    except OSError:
                        return
                    with self._lock:
                        self._faults += 1
                    # Half-close the upstream side so the broker reads the
                    # complete frame, then drop the client before any ack.
                    try:
                        upstream.shutdown(socket.SHUT_WR)
                    except OSError:
                        pass
                    return
                try:
                    upstream.sendall(packet)
                except OSError:
                    return

    def _is_command_publish(self, packet):
        if packet[0] >> 4 != PUBLISH:
            return False
        length, index = _varint(packet, 1)
        if length is None:
            return False
        body = packet[index : index + length]
        if len(body) < 2:
            return False
        topic_length = int.from_bytes(body[:2], "big")
        topic = body[2 : 2 + topic_length]
        if topic != self.topic:
            return False
        # A QoS>0 PUBLISH carries a two-byte packet identifier after the topic;
        # MQTT 5 then carries a varint property length and its properties.
        qos = (packet[0] >> 1) & 0x03
        index = 2 + topic_length + (2 if qos else 0)
        properties, index = _varint(body, index)
        if properties is None or index + properties > len(body):
            return False
        if _host_request_kind(body[index + properties :]) != COMMAND_REQUEST_KIND:
            return False
        with self._lock:
            return self._armed


def configure_requests(scenario, *, broker_url=None, **tunables):
    """Append a requests block and optionally point Access at a proxy.

    Rewriting the configuration before Access starts is the same supported
    configuration seam the TLS cases use. Tunables are Access YAML durations
    and integers; they shorten the retry loop without changing product code.
    """
    text = scenario.config.read_text()
    if broker_url is not None:
        original = '  url: "mqtt://127.0.0.1:1883"'
        assert text.count(original) == 1, "the scenario must use the default broker URL"
        text = text.replace(original, f'  url: "{broker_url}"', 1)
    assert "\nrequests:\n" not in text, "requests must be configured at most once"
    lines = ["requests:"]
    for key, value in tunables.items():
        lines.append(f"  {key}: {value}")
    scenario.config.write_text(text.rstrip("\n") + "\n" + "\n".join(lines) + "\n")
