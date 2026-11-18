"""Two independent native Hosts and one authenticated device."""
import pytest
import os
import subprocess
import json
from environment import ROOT, http, wait
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__('native-dsh')


def test_two_native_hosts_colliding_session_ids_require_explicit_routing(scenario, paired_device):
    """Identical source IDs across two native Hosts are ambiguous unless an authorized Host is selected."""
    directory = scenario.directory / 'second-native'
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = 'host-integration-2'
    other.http_port = 3110
    other.env = scenario.env.copy()
    try:
        other.start_native()
        invitation = http('/v1/pairing/invitations', {'hostId':other.host}, scenario.admin)
        paired_device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation['pairingUri'])
        assert scenario.request('test-enable-create')['enabled']
        assert other.request('test-enable-create')['enabled']
        assert scenario.request('test-enable-gx')['enabled']
        assert other.request('test-enable-gx')['enabled']
        assert scenario.request('gx-enable-multihost-queue')['enabled']
        assert other.request('gx-enable-multihost-queue')['enabled']
        shared = 'fixture-shared-session'
        first = paired_device.call('session', 'create', '--host', scenario.host, '--session-id', shared)['lifecycle_receipt']['session_id']
        second = paired_device.call('session', 'create', '--host', other.host, '--session-id', shared)['lifecycle_receipt']['session_id']
        assert first == second == shared
        assert any(x['session_id'] == shared for x in paired_device.sessions(scenario.host))
        assert any(x['session_id'] == shared for x in paired_device.sessions(other.host))
        assert paired_device.call("status")["default_host"] == other.host
        # With no saved default, identical Session IDs cannot select a Host.
        (paired_device.data_dir / "default-host.json").unlink()
        code, values = paired_device.invoke('session', 'interrupt', shared, success=False)
        assert code != 0 and values[-1]['error']['code'] == 'conflict'
        scenario.model.plan()
        result = paired_device.call('session', 'send', shared, '--delivery', 'start-if-idle', 'synthetic route', '--host', scenario.host, "--detach")
        assert result["target"] == {"host_id": scenario.host, "session_id": shared}
        paired_device.confirm_detached_delivery(result, shared, "synthetic route", "start-if-idle")
        scenario.wait_idle(shared)
        assert scenario.model.calls == 1 and other.model.calls == 0
        other.model.plan()
        result = paired_device.call('session', 'send', shared, '--delivery', 'start-if-idle', 'synthetic route', '--host', other.host, "--detach")
        assert result["target"] == {"host_id": other.host, "session_id": shared}
        paired_device.confirm_detached_delivery(result, shared, "synthetic route", "start-if-idle")
        other.wait_idle(shared)
        assert scenario.model.calls == 1 and other.model.calls == 1
        for host in (scenario.host, other.host):
            assert 'default_is_fallback' in paired_device.call('session', 'model', 'get', '--session', shared, '--host', host)
            code, output = paired_device.invoke('session', 'queue', 'list', '--session', shared, '--host', host, success=False)
            assert code == 0 and output[-1]['data']['session_id'] == shared
            assert paired_device.call('session', 'wait', shared, '--timeout', '3', '--host', host)['reason'] == 'idle'
            assert paired_device.call('session', 'show', shared, '--host', host)['session']['session_id'] == shared
            assert isinstance(paired_device.call('session', 'question', 'list', '--session', shared, '--host', host)['batches'], list)
        for target in (scenario, other):
            target.model.plan()
            process = subprocess.Popen(
                paired_device.args("session", "watch", shared, "--host", target.host,
                                   "--once", "--timeout", "15"),
                cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True)
            try:
                wait(lambda: target.request('watches') > 0, timeout=10)
                target.request('start', sessionId=shared)
                output, _ = process.communicate(timeout=20)
                assert process.returncode == 0
                envelopes = [json.loads(line) for line in output.splitlines() if line.strip()]
                assert any(value.get('data', {}).get('event', {}).get('session', {}).get('host_id') == target.host for value in envelopes)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=5)
        assert other.request('gx-fail-session-list')['armed']
        code, values = paired_device.invoke('session', 'interrupt', shared, success=False)
        assert code != 0 and values[-1]['error']['code'] == 'conflict'
        assert paired_device.call('session', 'wait', shared, '--timeout', '3', '--host', scenario.host)['reason'] == 'idle'
        assert other.request('gx-restore-session-list')['restored']
        fork = paired_device.call('session', 'fork', shared, '--host', other.host)['lifecycle_receipt']
        assert fork['parent_session_id'] == shared and fork['session_id'] != shared
        assert any(row['session_id'] == fork['session_id'] for row in paired_device.sessions(other.host))
        assert all(row['session_id'] != fork['session_id'] for row in paired_device.sessions(scenario.host))
        code, values = paired_device.invoke('session', 'interrupt', shared, '--host', 'fixture-unauthorized', success=False)
        assert code != 0 and values[-1]['error']['code'] == 'not_found'
        scenario.native.close()
        scenario.start_native()
        other.native.close()
        other.start_native()
        code, values = paired_device.invoke('session', 'interrupt', shared, success=False)
        assert code != 0 and values[-1]['error']['code'] == 'conflict'
        assert other.request('gx-revoke-host-grant', deviceId=paired_device.identity['device_id'])['revoked']
        wait(lambda: other.host not in {row['host_id'] for row in paired_device.hosts()}, timeout=20)
        code, values = paired_device.invoke('session', 'interrupt', shared, '--host', other.host, success=False)
        assert code == 4 and values[-1]['error']['code'] == 'forbidden'
        scenario.model.plan()
        accepted = paired_device.call('session', 'send', shared, '--delivery', 'start-if-idle', 'synthetic route', "--detach")
        assert accepted['receipt']['status'] == 'succeeded'
    finally:
        other.close()
