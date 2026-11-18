"""Secondary native Host FINISH response loss under real Access."""
import json
import socket
import threading
import urllib.parse
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import pytest
import subprocess
from pathlib import Path
from environment import Device, http
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__('native-dsh')


@pytest.mark.parametrize('revoke_host', [
    pytest.param(False, marks=pytest.mark.description('A cleaned pairing with a durably confirmed Host COMMIT recovers the original device credential.')),
    pytest.param(True, marks=pytest.mark.description('A cleaned committed pairing with a revoked grant remains available as redacted evidence.')),
])
def test_committed_outcome_survives_access_cleanup(revoke_host, scenario):
    """A minimal authenticated outcome survives cleanup independently of the invitation and pairing operation."""
    _exercise_committed_outcome_cleanup(revoke_host, False, scenario)


def test_confirmed_outcome_recovers_when_commit_body_is_cleaned_first(scenario):
    """A confirmed pairing recovers when its outbox body is gone but its operation remains."""
    _exercise_committed_outcome_cleanup(False, True, scenario)


def test_unproven_legacy_outcome_preserves_pending_credential(scenario):
    """An unproven active legacy confirmation remains unknown without deleting pending credentials."""
    _exercise_committed_outcome_cleanup(False, True, scenario, unproven=True)


def test_revoked_unproven_legacy_outcome_is_unavailable(scenario):
    """An inactive exact legacy pairing remains unavailable without claiming Host confirmation."""
    _exercise_committed_outcome_cleanup(True, True, scenario, unproven=True)


def test_legacy_active_commit_without_durable_ack_recovers_without_new_grant(scenario):
    """An active legacy pairing can recover by replaying its retained valid COMMIT after ACK loss."""
    _exercise_committed_outcome_cleanup(False, True, scenario, unproven=True, replay_commit=True)


def _exercise_committed_outcome_cleanup(revoke_host, retain_operation, scenario, unproven=False, replay_commit=False):
    def stage(value):
        if retain_operation:
            Path('/reports/commit-body-first-stage.json').write_text(json.dumps({'stage': value}))
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
        stage('primary-paired')
        allow_finish.clear()
        code, _ = device.invoke('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation(other.host), success=False)
        assert lost.is_set() and code != 0
        stage('finish-response-lost')
        if revoke_host:
            assert other.request('gx-revoke-host-grant', deviceId=device.identity['device_id'])['revoked']
            from environment import wait
            wait(lambda: other.host not in {row['host_id'] for row in device.hosts()},timeout=20)
        # Equivalent persisted state after Access removes expired operation and invitation.
        if retain_operation:
            # Model a persisted state with the operation retained after request body cleanup.
            if replay_commit:
                # Preserve the retained COMMIT while simulating a lost durable
                # ACK. An unknown result can be recovered explicitly without
                # putting the row back into the received-only forward queue.
                assert other.request('arm-pair-commit-replay-count')['armed']
                query = f"UPDATE {scenario.schema}.requests SET result_body=''::bytea,status='unknown',forward_state='none' WHERE request_id=(SELECT commit_request_id FROM {scenario.schema}.pairing_operations WHERE host_id='{other.host}' AND device_id='{device.identity['device_id']}')"
            else:
                query = f"UPDATE {scenario.schema}.requests SET request_body=''::bytea, result_body=''::bytea,bodies_cleared=true WHERE request_id=(SELECT commit_request_id FROM {scenario.schema}.pairing_operations WHERE host_id='{other.host}' AND device_id='{device.identity['device_id']}')"
            if unproven:
                query += f"; UPDATE {scenario.schema}.pairing_outcomes SET host_confirmed=true,commit_binding_verified=false,host_confirmation_verified=false WHERE host_id='{other.host}' AND device_id='{device.identity['device_id']}'"
        else:
            query = f"DELETE FROM {scenario.schema}.pairing_operations WHERE host_id='{other.host}' AND device_id='{device.identity['device_id']}'; DELETE FROM {scenario.schema}.pairing_tokens WHERE host_id='{other.host}'"
        subprocess.run(['psql','-h','127.0.0.1','-p','55432','-U','postgres','-d','whalerein_test','-c',query],check=True,capture_output=True,timeout=5)
        stage('body-cleared')
        allow_finish.set()
        if not revoke_host:
            operation_id, _ = device.call('operation', 'list')['pending_pairing']
        if replay_commit:
            stage('replay-generation-start')
            generation_query = f"SELECT grant_generation FROM {scenario.schema}.grants WHERE host_id='{other.host}' AND device_id='{device.identity['device_id']}'"
            def generation():
                return subprocess.run(['psql','-h','127.0.0.1','-p','55432','-U','postgres','-d','whalerein_test','-tAc',generation_query],check=True,capture_output=True,text=True,timeout=5).stdout.strip()
            before = generation()
            assert other.request('pair-commit-replay-count')['calls'] == 0
            stage('replay-resume-start')
            outcome = device.call('operation', 'recover', operation_id)['connection']
            stage('replay-resume-success')
            assert outcome['host_id'] == other.host
            assert generation() == before
            assert other.request('pair-commit-replay-count')['calls'] == 1
            assert other.host_peer_exists(device.identity['device_id'])
        elif unproven and not revoke_host:
            pending = device.call('operation', 'list')['pending_pairing']
            operation_id, pending_host = pending
            assert pending_host == other.host
            for _ in range(2):
                code, values = device.invoke('operation', 'recover', operation_id, success=False)
                assert code != 0 and values[-1]['error']['code'] == 'pairing_committed_unconfirmed'
                assert device.call('operation', 'list')['pending_pairing'] == pending
                assert device.call('status')['default_host'] == scenario.host
            assert scenario.host in {row['host_id'] for row in device.hosts()}
        elif not revoke_host:
            stage('resume-start')
            outcome = device.call('operation', 'recover', operation_id)['connection']
            stage('resume-success')
            assert outcome['host_id'] == other.host
            assert device.call('operation', 'list')['pairing_history'] == []
        else:
            outcome = device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation(other.host))
            assert outcome['host_id'] == other.host
            history = device.call('operation', 'list')['pairing_history']
            assert len(history) == 1 and history[0]['host_id'] == other.host and history[0]['claim_id']
            assert history[0]['status'] == 'committed_unavailable'
        assert {scenario.host, other.host} <= {row['host_id'] for row in device.hosts()}
    finally:
        other.close()
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=5)
