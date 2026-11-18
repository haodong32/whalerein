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


@pytest.mark.parametrize('drop_finish', [
    pytest.param(False, marks=pytest.mark.description('A locally revoked primary Host is re-paired without losing the other Host.')),
    pytest.param(True, marks=pytest.mark.description('A primary re-pair with lost FINISH replies resumes after restart while preserving the other Host.')),
])
def test_primary_host_repair_preserves_other_grant(drop_finish, scenario):
    """A revoked primary Host can be paired again while its other Host stays authorized."""
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
        device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation(other.host))
        assert scenario.request('gx-revoke-host-grant', deviceId=device.identity['device_id'])['revoked']
        from environment import wait
        wait(lambda: scenario.host not in {row['host_id'] for row in device.hosts()},timeout=20)
        if drop_finish: allow_finish.clear()
        code, values = device.invoke('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation(scenario.host), success=False)
        if drop_finish:
            assert lost.is_set() and code != 0
            assert other.host in {row['host_id'] for row in device.hosts()}
            allow_finish.set()
            operation_id, _ = device.call('operation', 'list')['pending_pairing']
            outcome = device.call('operation', 'recover', operation_id)['connection']
        else:
            assert code == 0
            outcome = values[-1]['data']
        assert outcome['host_id'] == scenario.host
        assert {scenario.host, other.host} <= {row['host_id'] for row in device.hosts()}
        assert device.sessions(scenario.host) == []
        assert device.sessions(other.host) == []
    finally:
        other.close()
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=5)
