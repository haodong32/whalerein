"""Global device revoke interleaved with a secondary Host pairing."""
import json
import os
import subprocess
import threading
import urllib.parse
import urllib.request
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import pytest
from environment import Device, ROOT, http
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__('native-dsh')


def test_secondary_finish_cannot_reactivate_after_global_revoke(scenario):
    """A globally revoked device cannot gain a new Host grant from an earlier pending FINISH."""
    entered = threading.Event()
    release = threading.Event()
    armed = threading.Event()
    class Proxy(BaseHTTPRequestHandler):
        def log_message(self, *_args): pass
        def forward(self):
            upstream = HTTPConnection('127.0.0.1', 18080, timeout=40)
            try:
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                claim = json.loads(body) if self.path == '/v1/pairing/claims' else {}
                if armed.is_set() and claim.get('phase') == 'PAIRING_PHASE_FINISH':
                    entered.set()
                    if not release.wait(30): raise OSError('fixture gate timed out')
                headers = {key:value for key,value in self.headers.items() if key.lower() not in {'host','connection'}}
                upstream.request(self.command, self.path, body, headers)
                response = upstream.getresponse()
                payload = response.read()
                self.send_response(response.status)
                self.send_header('Content-Type','application/json')
                self.send_header('Content-Length',str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except (ConnectionError, OSError, ValueError): self.close_connection = True
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
    process = None
    def invitation(host):
        raw = http('/v1/pairing/invitations', {'hostId':host}, scenario.admin)['pairingUri']
        uri = urllib.parse.urlsplit(raw)
        query = dict(urllib.parse.parse_qsl(uri.query))
        query['access'] = f'http://127.0.0.1:{proxy.server_port}'
        return urllib.parse.urlunsplit(uri._replace(query=urllib.parse.urlencode(query)))
    def counts(device):
        query = f"SELECT count(*) FILTER (WHERE revoked_at IS NULL) FROM {scenario.schema}.grants WHERE device_id='{device.identity['device_id']}'"
        result = subprocess.run(['psql','-h','127.0.0.1','-p','55432','-U','postgres','-d','whalerein_test','-tAc',query],check=True,capture_output=True,text=True,timeout=5)
        return int(result.stdout.strip())
    try:
        other.start_native()
        device = Device(scenario.directory)
        device.identity = device.call('pair', invitation(scenario.host))
        secondary_uri = invitation(other.host)
        token = device.transport_token()
        armed.set()
        process = subprocess.Popen([str(ROOT/'target/debug/whalerein'),'--json','--yes','--data-dir',str(device.directory/'client'),'pair',secondary_uri],cwd=ROOT,env={**os.environ,'WHALEREIN_SECRET_BACKEND':'file:'+str(device.directory/'secrets'),'WHALEREIN_LOG':'error'},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        assert entered.wait(20)
        request = urllib.request.Request(f'http://127.0.0.1:18080/v1/devices/{device.identity["device_id"]}',headers={'Authorization':'Bearer '+token},method='DELETE')
        with urllib.request.urlopen(request,timeout=10) as response: assert response.status in (200,204)
        assert counts(device) == 0
        release.set()
        process.communicate(timeout=30)
        live = counts(device)
        Path('/reports/pair-revoke-stage').write_text(f'finish_exit={process.returncode} live_grants={live}')
        assert live == 0
        assert process.returncode != 0
    finally:
        release.set()
        if process and process.poll() is None:
            process.kill()
            process.communicate(timeout=5)
        other.close()
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=5)
