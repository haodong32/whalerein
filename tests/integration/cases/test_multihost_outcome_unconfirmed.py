"""A missing durable Host COMMIT proof is never turned into success."""
import subprocess
import pytest
from environment import http, wait
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__('native-dsh')


def test_cleaned_unconfirmed_commit_retains_pending_until_host_revocation(scenario, paired_device):
    """A lost Host confirmation write remains unknown after cleanup and later evolves safely on Host revocation."""
    directory = scenario.directory / 'second-native'
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = 'host-integration-2'
    other.http_port = 3110
    other.env = scenario.env.copy()
    def sql(statement):
        subprocess.run(['psql','-h','127.0.0.1','-p','55432','-U','postgres','-d','whalerein_test','-v','ON_ERROR_STOP=1','-c',f'SET search_path TO {scenario.schema}; {statement}'],check=True,capture_output=True,timeout=5)
    def invitation():
        return http('/v1/pairing/invitations', {'hostId':other.host}, scenario.admin)['pairingUri']
    try:
        other.start_native()
        sql("CREATE FUNCTION fail_confirmation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.host_confirmed THEN RAISE EXCEPTION 'fixture'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_confirmation BEFORE UPDATE ON pairing_outcomes FOR EACH ROW EXECUTE FUNCTION fail_confirmation()")
        code, _ = paired_device.invoke('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation(), success=False)
        assert code != 0
        sql("DROP TRIGGER fail_confirmation ON pairing_outcomes; DROP FUNCTION fail_confirmation(); DELETE FROM pairing_operations WHERE host_id='host-integration-2'; DELETE FROM pairing_tokens WHERE host_id='host-integration-2'")
        for _ in range(2):
            operation_id, _ = paired_device.call('operation', 'list')['pending_pairing']
            code, result = paired_device.invoke('operation', 'recover', operation_id, success=False)
            assert code != 0 and result[-1]['error']['code'] == 'pairing_committed_unconfirmed'
        history = paired_device.call('operation', 'list')['pairing_history']
        assert len(history) == 1 and history[0]['status'] == 'committed_host_confirmation_unknown'
        assert other.request('gx-revoke-host-grant', deviceId=paired_device.identity['device_id'])['revoked']
        wait(lambda: other.host not in {row['host_id'] for row in paired_device.hosts()},timeout=20)
        assert paired_device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation())['host_id'] == other.host
        history = paired_device.call('operation', 'list')['pairing_history']
        assert len(history) == 1 and history[0]['status'] == 'committed_unavailable'
        assert {scenario.host,other.host} <= {row['host_id'] for row in paired_device.hosts()}
    finally:
        other.close()
