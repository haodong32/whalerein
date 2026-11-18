"""A late Host COMMIT acknowledgement survives Access cleanup."""
import json
import os
import subprocess
import pytest
from pathlib import Path
from environment import ROOT, http, wait
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__('native-dsh')


def test_late_pairing_ack_recovers_after_cleanup_without_client_retry(scenario, paired_device):
    """A Host COMMIT acknowledged after the transient waiter times out is retained for later recovery."""
    directory = scenario.directory / 'second-native'
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = 'host-integration-2'
    other.http_port = 3110
    other.env = scenario.env.copy()
    process = None
    released = False
    def sql(statement):
        result=subprocess.run(['psql','-h','127.0.0.1','-p','55432','-U','postgres','-d','whalerein_test','-tAc',f'SET search_path TO {scenario.schema}; {statement}'],check=True,capture_output=True,text=True,timeout=5)
        return result.stdout.strip()
    try:
        scenario.access.close()
        scenario.config.write_text(scenario.config.read_text() + "\npairing:\n  claim_timeout: 2s\n")
        scenario.start_access()
        other.start_native()
        invite=http('/v1/pairing/invitations',{'hostId':other.host},scenario.admin)['pairingUri']
        assert other.request('gx-hold-pair-commit-result')['armed']
        process = subprocess.Popen(paired_device.args("host", "connect", "--invite-stdin", "--yes"), cwd=ROOT, env=paired_device.env(), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        process.stdin.write(invite)
        process.stdin.close()
        process.stdin = None
        wait(lambda:other.request('gx-held-pair-commit-result')['entered'],timeout=20)
        Path('/reports/late-stage').write_text('held')
        output,_=process.communicate(timeout=40)
        Path('/reports/late-stage').write_text(f"first-exit={process.returncode} code={json.loads(output).get('error',{}).get('code')}")
        assert process.returncode != 0 and json.loads(output)['error']['code']=='unavailable'
        assert other.request('gx-release-pair-commit-result')['released']
        released=True
        wait(lambda:sql("SELECT status FROM requests WHERE host_id='host-integration-2' AND request_id LIKE 'pair-commit-%' ORDER BY created_at DESC LIMIT 1").endswith('succeeded'),timeout=20)
        Path('/reports/late-stage').write_text('ack-stored')
        sql("DELETE FROM pairing_operations WHERE host_id='host-integration-2'; DELETE FROM pairing_tokens WHERE host_id='host-integration-2'; UPDATE requests SET request_body=''::bytea,result_body=''::bytea WHERE host_id='host-integration-2' AND request_id LIKE 'pair-commit-%'")
        Path('/reports/late-stage').write_text('cleaned')
        operation_id, _ = paired_device.call('operation', 'list')['pending_pairing']
        code,values=paired_device.invoke('operation', 'recover', operation_id,success=False)
        Path('/reports/late-stage').write_text(f"resume-exit={code} code={values[-1].get('error',{}).get('code')}")
        assert code==0
        recovered=values[-1]['data']['connection']
        assert recovered['host_id']==other.host
        assert {scenario.host,other.host}<={row['host_id'] for row in paired_device.hosts()}
    finally:
        if not released:
            try: other.request('gx-release-pair-commit-result')
            except RuntimeError: pass
        if process and process.poll() is None: process.kill();process.communicate(timeout=5)
        other.close()
