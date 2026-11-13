"""Creation-specific security and independent-device acceptance."""
import pytest
from environment import Device, http

pytestmark = pytest.mark.chain


def test_creation_unauthorized_clients_do_not_invoke_source(scenario, paired_device):
    """Unpaired and revoked devices cannot create a source session or receive its identity."""
    before = scenario.dsh.process.control('gx_effect_counts')['executeHost']
    stranger = Device(scenario.directory / 'create-stranger')
    code, values = stranger.invoke('session-create', '--host', scenario.host, success=False)
    assert code != 0 and all('lifecycle_receipt' not in value.get('data', {}) for value in values)
    assert scenario.dsh.process.control('gx_effect_counts')['executeHost'] == before
    scenario.revoke_from_host(paired_device)
    code, values = paired_device.invoke('session-create', '--host', scenario.host, success=False)
    assert code != 0 and all('lifecycle_receipt' not in value.get('data', {}) for value in values)
    assert scenario.dsh.process.control('gx_effect_counts')['executeHost'] == before


def test_creation_two_authorized_devices_get_independent_sessions(scenario, paired_device):
    """Two independently paired devices create distinct source sessions in the same default location."""
    other = Device(scenario.directory / 'second-device')
    invitation = http('/v1/pairing/invitations', {'hostId':scenario.host}, scenario.admin)
    other.identity = other.call('pair', invitation['pairingUri'])
    assert other.identity['device_id'] != paired_device.identity['device_id']
    before = scenario.dsh.process.control('gx_effect_counts')['executeHost']
    first = paired_device.call('session-create', '--host', scenario.host)['lifecycle_receipt']['session_id']
    second = other.call('session-create', '--host', scenario.host)['lifecycle_receipt']['session_id']
    assert first and second and first != second
    assert scenario.dsh.process.control('gx_effect_counts')['executeHost'] == before + 2
    assert {first, second} <= {item['session_id'] for item in paired_device.sessions(scenario.host)}


def test_creation_wrong_host_route_does_not_replay_source(scenario, paired_device):
    """A captured create request cannot execute through a different Host route."""
    receipt = paired_device.call('session-create', '--host', scenario.host)['lifecycle_receipt']
    before = scenario.dsh.process.control('gx_effect_counts')['executeHost']
    with pytest.raises(RuntimeError, match=r'HTTP (400|401|403|404)'):
        scenario.replay_command(paired_device, route_host_override='fixture-other-host')
    assert scenario.dsh.process.control('gx_effect_counts')['executeHost'] == before
    assert receipt['session_id']


def test_creation_refresh_failure_does_not_repeat_create(scenario, paired_device):
    """A failed session-list refresh keeps the confirmed identity and does not repeat creation."""
    before = scenario.dsh.process.control('gx_effect_counts')['executeHost']
    receipt = paired_device.call('session-create', '--host', scenario.host)['lifecycle_receipt']
    identity = receipt['session_id']
    assert scenario.dsh.process.control('fail_next_session_refresh')
    code, _ = paired_device.invoke('sessions', '--host', scenario.host, success=False)
    assert code != 0
    assert scenario.dsh.process.control('gx_effect_counts')['executeHost'] == before + 1
    assert any(item['session_id'] == identity for item in paired_device.sessions(scenario.host))
    assert scenario.dsh.process.control('gx_effect_counts')['executeHost'] == before + 1
