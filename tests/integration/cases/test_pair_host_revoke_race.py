"""Global device revoke interleaved with a secondary Host pairing."""
import json
import os
import subprocess
import socket
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


@pytest.mark.parametrize('drop_fenced', [
    pytest.param(False, marks=pytest.mark.description('A repeated Host revoke fences an earlier FINISH and a new invitation succeeds.')),
    pytest.param(True, marks=pytest.mark.description('A lost fenced response retains pending recovery until a new invitation safely resolves it.')),
])
def test_existing_host_finish_cannot_reactivate_after_host_revoke(drop_fenced, scenario):
    """A Host-revoked grant cannot be restored by an earlier pending FINISH while another Host remains authorized."""
    entered = threading.Event()
    release = threading.Event()
    armed = threading.Event()
    lost = threading.Event()
    lose_fenced = threading.Event()
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
                if lose_fenced.is_set() and claim.get('phase') == 'PAIRING_PHASE_FINISH' and response.status == 409:
                    lost.set()
                    self.connection.shutdown(socket.SHUT_RDWR)
                    self.connection.close()
                    return
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
        device.call('pair', invitation(other.host))
        # A repeated Host revoke must fence a re-pair started after the first one.
        assert other.request('gx-revoke-host-grant', deviceId=device.identity['device_id'])['revoked']
        assert counts(device) == 1
        secondary_uri = invitation(other.host)
        armed.set()
        if drop_fenced: lose_fenced.set()
        process = subprocess.Popen([str(ROOT/'target/debug/whalerein'),'--json','--yes','--data-dir',str(device.directory/'client'),'pair',secondary_uri],cwd=ROOT,env={**os.environ,'WHALEREIN_SECRET_BACKEND':'file:'+str(device.directory/'secrets'),'WHALEREIN_LOG':'error'},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        assert entered.wait(20)
        assert other.request('gx-revoke-host-grant', deviceId=device.identity['device_id'])['revoked']
        assert counts(device) == 1
        release.set()
        output, _ = process.communicate(timeout=30)
        envelopes = [json.loads(line) for line in output.splitlines() if line.strip()]
        if drop_fenced:
            assert lost.is_set() and envelopes[-1]['error']['code'] != 'pairing_fenced'
        else:
            assert envelopes[-1]['error']['code'] == 'pairing_fenced'
        lose_fenced.clear()
        live = counts(device)
        Path('/reports/pair-revoke-stage').write_text(f'finish_exit={process.returncode} live_grants={live}')
        assert live == 1
        assert process.returncode != 0
        assert scenario.host in {row['host_id'] for row in device.hosts()}
        renewed = device.call('pair', invitation(other.host))
        assert renewed['host_id'] == other.host and counts(device) == 2
    finally:
        release.set()
        if process and process.poll() is None:
            process.kill()
            process.communicate(timeout=5)
        other.close()
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=5)
