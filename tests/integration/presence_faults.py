"""Deterministic MQTT presence-publish faults for Access recovery acceptance.

The fault is a real TCP/MQTT failure, not a mocked Access status: Access
connects to this loopback proxy as its broker, and when an armed ``request_stale``
presence PUBLISH appears the proxy drops the connection before forwarding the
packet. Access must then reconnect through the same proxy. Nothing in the
product is bypassed, so the observed release still comes from real Access and
Host behavior.
"""

import socket
import threading

PUBLISH = 3
REQUEST_STALE = b"request_stale"


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


def _packet(buffer):
    """Split one complete MQTT packet from the front of a stream buffer."""
    if len(buffer) < 2:
        return None
    length, index = _varint(buffer, 1)
    if length is None:
        return None
    end = index + length
    if end > len(buffer):
        return None
    return buffer[:end], buffer[end:]


def _publish(packet):
    """Return (topic, payload) for an MQTT 5 PUBLISH packet, else None."""
    if not packet or packet[0] >> 4 != PUBLISH:
        return None
    length, index = _varint(packet, 1)
    if length is None:
        return None
    body = packet[index : index + length]
    if len(body) < 2:
        return None
    topic_length = int.from_bytes(body[:2], "big")
    if 2 + topic_length > len(body):
        return None
    topic = body[2 : 2 + topic_length]
    offset = 2 + topic_length
    qos = (packet[0] >> 1) & 0x03
    if qos > 0:
        offset += 2  # packet identifier
    properties_length, index = _varint(body, offset)
    if properties_length is None:
        return None
    offset = index + properties_length
    if offset > len(body):
        return None
    return topic, body[offset:]


class PresencePublishFault:
    """One-shot ``request_stale`` presence publish failure in front of the broker."""

    def __init__(self, host_id, upstream=("127.0.0.1", 1883)):
        self.topic = f"whalerein/v1/h/{host_id}/presence".encode()
        self.upstream = upstream
        self._lock = threading.Lock()
        self._armed = False
        self._faults = 0
        self._connections = 0
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

    @property
    def connections(self):
        with self._lock:
            return self._connections

    def arm(self):
        """Fail the next ``request_stale`` presence publish and reconnect after."""
        with self._lock:
            self._armed = True
            self._faults = 0

    def close(self):
        with self._lock:
            self._armed = False
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
            with self._lock:
                self._connections += 1
            threading.Thread(target=self._handle, args=(client,), daemon=True).start()

    def _handle(self, client):
        try:
            upstream = socket.create_connection(self.upstream, timeout=10)
        except OSError:
            client.close()
            return
        state = {"faulted": False}

        def pump_upstream():
            while not state["faulted"]:
                try:
                    chunk = upstream.recv(65536)
                except OSError:
                    return
                if not chunk:
                    return
                try:
                    client.sendall(chunk)
                except OSError:
                    return

        reader = threading.Thread(target=pump_upstream, daemon=True)
        reader.start()
        try:
            self._pump_client(client, upstream, state)
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

    def _pump_client(self, client, upstream, state):
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
                if self._fail(packet):
                    with self._lock:
                        self._armed = False
                        self._faults += 1
                    state["faulted"] = True
                    # Drop the connection without forwarding the PUBLISH so
                    # Access' publish fails and it must reconnect.
                    try:
                        upstream.shutdown(socket.SHUT_WR)
                    except OSError:
                        pass
                    return
                try:
                    upstream.sendall(packet)
                except OSError:
                    return

    def _fail(self, packet):
        published = _publish(packet)
        if published is None:
            return False
        topic, payload = published
        if topic != self.topic or REQUEST_STALE not in payload:
            return False
        with self._lock:
            return self._armed
