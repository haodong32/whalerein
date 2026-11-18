"""Secondary native Host FINISH response loss under real Access."""
import json
import socket
import threading
import urllib.parse
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import pytest
from environment import Device, http
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__('native-dsh')


def test_global_revoke_redacts_pending_secondary_pairing(scenario):
    """A global revoke removes an uncertain secondary FINISH credential while retaining only its redacted identity."""
    allow_finish = threading.Event()
    allow_finish.set()
    lost = threading.Event()
    class Proxy(BaseHTTPRequestHandler):
        def log_message(self, *_args): pass
        def forward(self):
            upstream = HTTPConnection('127.0.0.1', 18080, timeout=25)
            try:
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                headers = {key:value for key,value in self.headers.items() if key.lower() not in {'host','connection'}}
                upstream.request(self.command, self.path, body, headers)
                response = upstream.getresponse()
                payload = response.read()
                claim = json.loads(body) if self.path == '/v1/pairing/claims' else {}
                if claim.get('phase') == 'PAIRING_PHASE_FINISH' and response.status == 200 and not allow_finish.is_set():
                    lost.set()
                    self.connection.shutdown(socket.SHUT_RDWR)
                    self.connection.close()
                    return
                self.send_response(response.status)
                self.send_header('Content-Type','application/json')
                self.send_header('Content-Length',str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except (ConnectionError, OSError, ValueError):
                self.close_connection = True
            finally: upstream.close()
        do_POST = forward
        do_GET = forward
        do_DELETE = forward
    proxy = ThreadingHTTPServer(('127.0.0.1',0), Proxy)
    thread = threading.Thread(target=proxy.serve_forever,daemon=True)
    thread.start()
    directory = scenario.directory / 'second-native'
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = 'host-integration-2'
    other.http_port = 3110
    other.env = scenario.env.copy()
    def invitation(host):
        raw = http('/v1/pairing/invitations', {'hostId':host}, scenario.admin)['pairingUri']
        uri = urllib.parse.urlsplit(raw)
        query = dict(urllib.parse.parse_qsl(uri.query))
        query['access'] = f'http://127.0.0.1:{proxy.server_port}'
        return urllib.parse.urlunsplit(uri._replace(query=urllib.parse.urlencode(query)))
    try:
        other.start_native()
        device = Device(scenario.directory)
        device.identity = device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation(scenario.host))
        allow_finish.clear()
        code, _ = device.invoke('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation(other.host), success=False)
        assert lost.is_set() and code != 0
        assert device.has_secret("pending-pairing") and device.has_secret("device-token")
        claim, pending_host = device.call("operation", "list")["pending_pairing"]
        assert pending_host == other.host
        revoked = device.call("device", "revoke", "--yes")
        assert revoked["revoked"] and not revoked["revoked_retention_unverified"]
        assert not device.has_secret("pending-pairing") and not device.has_secret("device-token")
        operations = device.call("operation", "list")
        assert operations["pending_pairing"] is None
        history = operations["pairing_history"]
        assert len(history) == 1
        assert history[0]["claim_id"] == claim
        assert history[0]["host_id"] == other.host
        assert history[0]["status"] == "unconfirmed_revoked"
        assert set(history[0]) == {"claim_id", "host_id", "fingerprint", "status"}
    finally:
        other.close()
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=5)
