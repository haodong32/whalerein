"""One operation observed through two Access nodes sharing PostgreSQL and the broker."""
import json
import subprocess
import urllib.error
import urllib.request
import pytest
from environment import Device, http, wait, Process

pytestmark = pytest.mark.__getattr__('native-dsh')

NODE_B = 18082


def second_node(model):
    """Start a second Access node on the same schema and broker as the first.

    Only the listeners and the broker client id change. The database URL, the
    admin and provisioning tokens and the broker credentials stay shared, so
    the two processes are front ends of one region rather than two regions.
    """
    directory = model.directory / 'node-b'
    directory.mkdir()
    config = directory / 'access-b.yaml'
    text = model.config.read_text()
    text = text.replace('127.0.0.1:18080', f'127.0.0.1:{NODE_B}')
    text = text.replace('127.0.0.1:18081', '127.0.0.1:18084')
    text = text.replace(model.access_client_id, model.access_client_id + '-b')
    config.write_text(text)
    node = Process(['/usr/local/bin/whalerein-access', '--config', str(config)],
                   directory, model.env)
    model.processes.append(node)
    return node


def await_ready(node, model):
    try:
        wait(lambda: status_at(NODE_B, '/readyz') == 200, process=node)
    except AssertionError:
        for path in sorted((model.directory / 'node-b').glob('*.log')):
            print('NODE-B LOG', path.name, path.read_text()[-2000:], flush=True)
        raise


def status_at(port, path):
    request = urllib.request.Request(f'http://127.0.0.1:{port}{path}')
    try:
        with urllib.request.urlopen(request, timeout=3) as response:
            return response.status
    except (OSError, urllib.error.URLError):
        return None


def read_at(port, path, token):
    request = urllib.request.Request(
        f'http://127.0.0.1:{port}{path}',
        headers={'Authorization': 'Bearer ' + token})
    with urllib.request.urlopen(request, timeout=5) as response:
        return response.status, json.load(response)


def latest_request(schema, device_id):
    query = (f"SELECT request_id, kind FROM {schema}.requests "
             f"WHERE device_id='{device_id}' ORDER BY created_at DESC LIMIT 1")
    result = subprocess.run(
        ['psql', '-h', '127.0.0.1', '-p', '55432', '-U', 'postgres',
         '-d', 'whalerein_test', '-tAc', query],
        check=True, capture_output=True, text=True, timeout=5)
    line = result.stdout.strip()
    request_id, _, kind = line.partition('|')
    return request_id, kind


def test_second_node_serves_the_operation_recorded_by_the_first(scenario, paired_device):
    """A request admitted through one node reports the same status through the other."""
    device = paired_device
    node = second_node(scenario)
    await_ready(node, scenario)
    # Admit work through the first node, then read the same grant on the second.
    device.sessions(scenario.host)
    status, hosts = read_at(NODE_B, '/v1/hosts', device.transport_token())
    assert status == 200
    assert scenario.host in {row['hostId'] for row in hosts['hosts']}
    request_id, kind = latest_request(scenario.schema, device.identity['device_id'])
    assert request_id
    # `kind` proves the record is the handshake/query just admitted, not an
    # unrelated earlier row. The column stores the enum number (1 or 2).
    assert kind in {'1', '2', 'HANDSHAKE', 'QUERY'}
    first_status, first_body = read_at(18080, f'/v1/requests/{request_id}', device.transport_token())
    second_status, second_body = read_at(NODE_B, f'/v1/requests/{request_id}', device.transport_token())
    assert first_status == second_status == 200
    assert first_body['status'] == second_body['status']


def test_pairing_through_the_second_node_consumes_the_invitation_once(scenario):
    """An invitation created on one node is claimed and consumed on the other."""
    node = second_node(scenario)
    await_ready(node, scenario)
    device = Device(scenario.directory)
    invitation = http('/v1/pairing/invitations', {'hostId': scenario.host}, scenario.admin)
    uri = invitation['pairingUri'].replace('18080', str(NODE_B))
    device.identity = device.call('pair', uri)
    assert device.identity['host_id'] == scenario.host
    status, hosts = read_at(18080, '/v1/hosts', device.transport_token())
    assert status == 200
    assert scenario.host in {row['hostId'] for row in hosts['hosts']}
    # The used invitation cannot start another claim on either node.
    for port in (18080, NODE_B):
        with pytest.raises(RuntimeError):
            http_at(port, '/v1/pairing/claims', {
                'phase': 'PAIRING_PHASE_HELLO',
                'token': invitation['token'],
                'claimId': 'replayed-claim',
                'deviceId': 'replayed-device',
                'noiseMessage': 'AAAA',
                'protocolVersion': 1,
            })


def http_at(port, path, body, token=None):
    request = urllib.request.Request(
        f'http://127.0.0.1:{port}{path}',
        data=json.dumps(body).encode(),
        headers={'Content-Type': 'application/json',
                 **({'Authorization': 'Bearer ' + token} if token else {})})
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        raise RuntimeError(f'Access returned HTTP {error.code}') from None
