"""Global device revoke while a second pairing is committing locally."""
import json
import subprocess
import pytest
from environment import ROOT, http, wait
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__('native-dsh')


def test_old_pairing_reply_cannot_restore_credentials_after_global_revoke(scenario, paired_device):
    """A second Host FINISH reply cannot restore local credentials after a global revoke commits."""
    directory=scenario.directory/'second-native';directory.mkdir()
    other=NativeScenario(directory,scenario.schema);other.host='host-integration-2';other.http_port=3110;other.env=scenario.env.copy()
    pair_process=None;revoke_process=None
    barrier=scenario.directory/'pair-before-config'
    env = paired_device.env()
    common = paired_device.args("--yes")
    def active_grants():
        query=f"SELECT count(*) FROM {scenario.schema}.grants WHERE device_id='{paired_device.identity['device_id']}' AND revoked_at IS NULL"
        return int(subprocess.run(['psql','-h','127.0.0.1','-p','55432','-U','postgres','-d','whalerein_test','-tAc',query],check=True,capture_output=True,text=True,timeout=5).stdout.strip())
    try:
        other.start_native()
        invite=http('/v1/pairing/invitations',{'hostId':other.host},scenario.admin)['pairingUri']
        pair_process=subprocess.Popen([*common, 'host', 'connect', '--invite-stdin'],cwd=ROOT,env={**env,'WHALEREIN_TEST_PAIR_BEFORE_CONFIG_BARRIER':str(barrier)},stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        pair_process.stdin.write(invite)
        pair_process.stdin.close()
        pair_process.stdin = None
        wait(barrier.exists,timeout=25)
        revoke_process=subprocess.Popen([*common, 'device', 'revoke'],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        wait(lambda:active_grants()==0,timeout=25)
        barrier.unlink()
        pair_output,_=pair_process.communicate(timeout=20)
        revoke_output,_=revoke_process.communicate(timeout=30)
        pair_result=json.loads(pair_output)
        revoke_result=json.loads(revoke_output)
        assert pair_process.returncode != 0 and pair_result['error']['code'] in ('revoked','unavailable')
        assert revoke_process.returncode == 0 and revoke_result['data']['revoked']
        status = paired_device.call("status")
        assert status["device_access"] == "revoked"
        operations = paired_device.call("operation", "list")
        assert operations["pending_pairing"] is None
        history = operations["pairing_history"]
        assert len(history)==1 and history[0]['status']=='unconfirmed_revoked'
    finally:
        if barrier.exists():barrier.unlink()
        for process in (pair_process,revoke_process):
            if process and process.poll() is None:process.kill();process.communicate(timeout=5)
        other.close()


def test_forget_cannot_replace_identity_during_revocation_cleanup(scenario, paired_device):
    """A concurrent forget cannot cross an active global cleanup admission lock."""
    barrier = scenario.directory / 'revoke-before-cleanup'
    env = paired_device.env()
    common = paired_device.args("--yes")
    process = subprocess.Popen([*common, 'device', 'revoke'], cwd=ROOT, env={**env, 'WHALEREIN_TEST_REVOCATION_BARRIER': str(barrier)}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(barrier.exists, timeout=25)
        forget = subprocess.run([*common, 'device', 'forget', '--local-only'], cwd=ROOT, env=env, capture_output=True, text=True, timeout=15)
        assert forget.returncode != 0
        barrier.unlink()
        output, _ = process.communicate(timeout=30)
        assert process.returncode == 0 and json.loads(output)['data']['revoked']
        forgotten = subprocess.run([*common, 'device', 'forget', '--local-only'], cwd=ROOT, env=env, capture_output=True, text=True, timeout=15)
        assert forgotten.returncode == 0
        invitation = http('/v1/pairing/invitations', {'hostId': scenario.host}, scenario.admin)['pairingUri']
        paired = subprocess.run([*common, 'host', 'connect', '--invite-stdin'], cwd=ROOT, env=env, input=invitation, capture_output=True, text=True, timeout=25)
        assert paired.returncode == 0
        new_device = json.loads(paired.stdout)['data']['device_id']
        assert new_device != paired_device.identity['device_id']
        assert (paired_device.data_dir / 'regional.json').exists()
    finally:
        if barrier.exists(): barrier.unlink()
        if process.poll() is None: process.kill(); process.communicate(timeout=5)


def test_revoke_then_ordinary_forget_allows_a_new_pairing(scenario, paired_device):
    """After a global revoke, an ordinary forget clears the identity so the next invitation issues a new device."""
    env = paired_device.env()
    common = paired_device.args("--yes")
    revoked = subprocess.run([*common, 'device', 'revoke'], cwd=ROOT, env=env, capture_output=True, text=True, timeout=30)
    assert revoked.returncode == 0 and json.loads(revoked.stdout)['data']['revoked']
    forgotten = subprocess.run([*common, 'device', 'forget'], cwd=ROOT, env=env, capture_output=True, text=True, timeout=15)
    assert forgotten.returncode == 0
    assert json.loads(forgotten.stdout)['data'] == {
        'identity_forgotten': True,
        'local_only': False,
        'remote_grant_may_remain': False,
    }
    assert not (paired_device.data_dir / 'regional.json').exists()
    invitation = http('/v1/pairing/invitations', {'hostId': scenario.host}, scenario.admin)
    paired = subprocess.run([*common, 'host', 'connect', '--invite-stdin'], cwd=ROOT, env=env, input=invitation['pairingUri'], capture_output=True, text=True, timeout=25)
    assert paired.returncode == 0
    assert json.loads(paired.stdout)['data']['device_id'] != paired_device.identity['device_id']
    assert (paired_device.data_dir / 'regional.json').exists()
