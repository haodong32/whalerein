"""One operation observed through two Access nodes sharing PostgreSQL and the broker.

The first two cases drive the original Host chain through the native DSH
fixture. The AC-09 relay cases below drive the same isolated two-node Access
region over its public HTTP directory surface and a real Device bearer; they
use the case fixture Host/Device identity rather than reading private tables.
"""
import base64
import hashlib
import json
import subprocess
import time
import urllib.error
import urllib.request
import pytest
from environment import Device, Process, RelayDirectory, http, wait

native_dsh = pytest.mark.__getattr__('native-dsh')

NODE_B = 18082
NODE_A_OWNER = "node-integration"
NODE_B_OWNER = "node-b-integration"


def second_node(model, owner=None):
    """Start a second Access node on the same schema and broker as the first.

    Only the listeners and the broker client id change. The database URL, the
    admin and provisioning tokens and the broker credentials stay shared, so
    the two processes are front ends of one region rather than two regions.

    When ``owner`` is given the second node gets its own storage root and
    owner/node identity. It is then a genuinely different owner in the same
    region: it can store its own objects but must refuse another owner's
    route. Its cleanup and reconciliation stay at their configured cadence:
    maintenance is owner-scoped, so it must never fence the first node's rows.
    """
    directory = model.directory / 'node-b'
    directory.mkdir()
    config = directory / 'access-b.yaml'
    text = model.config.read_text()
    text = text.replace('127.0.0.1:18080', f'127.0.0.1:{NODE_B}')
    text = text.replace('127.0.0.1:18081', '127.0.0.1:18084')
    text = text.replace(model.access_client_id, model.access_client_id + '-b')
    if owner is not None:
        root = directory / 'payloads'
        root.mkdir()
        rewritten = []
        for line in text.splitlines():
            if line.startswith('  root_dir:'):
                line = '  root_dir: ' + json.dumps(str(root))
            elif line.startswith('  node_id:'):
                line = f'  node_id: "{owner}"'
            elif line.startswith('  owner_id:'):
                line = f'  owner_id: "{owner}"'
            rewritten.append(line)
        text = '\n'.join(rewritten) + '\n'
    config.write_text(text)
    node = Process(['/workspace/.verification/bin/whalerein-access', '--config', str(config)],
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


def ciphertext(size, seed=0):
    return bytes((seed + index * 13) & 0xFF for index in range(size))


def header(headers, name):
    for key, value in headers.items():
        if key.lower() == name.lower():
            return value
    raise AssertionError(f'missing relay response header {name}')


def error(payload):
    try:
        return json.loads(payload)
    except (TypeError, ValueError):
        return {}


def directory_at(port):
    return RelayDirectory(origin=f'http://127.0.0.1:{port}')


def reserve_at(directory, token, scenario, device, payload, **overrides):
    scope = {
        "host_id": scenario.host,
        "device_id": device.identity["device_id"],
        "channel_id": "channel-two-node",
        "purpose": "PAYLOAD_PURPOSE_RESULT",
        "ciphertext": payload,
    }
    scope.update(overrides)
    return directory.reserve(token, **scope)


def store_at(directory, token, scenario, device, payload, *, owner_node=None, **overrides):
    status, _, body = reserve_at(
        directory, token, scenario, device, payload,
        owner_node=owner_node, **overrides,
    )
    assert status == 200, body
    reservation = json.loads(body)
    status, _, body = directory.upload(
        token, reservation, payload, owner_node=owner_node)
    assert status == 204, body
    status, _, body = directory.finish(
        token, reservation["objectId"], payload, owner_node=owner_node)
    assert status == 200, body
    return reservation, json.loads(body)


def assert_exact(status, headers, body, payload):
    assert status == 200
    assert body == payload
    assert int(header(headers, 'Content-Length')) == len(payload)
    expected = 'sha-256=' + base64.b64encode(hashlib.sha256(payload).digest()).decode()
    assert header(headers, 'Digest') == expected
    assert int(header(headers, 'WhaleRein-Object-Expires-At-Unix-Ms')) > 0


def assert_refused(status, body, payload):
    assert status != 200, body
    assert body != payload


@native_dsh
def test_second_node_serves_the_operation_recorded_by_the_first(scenario, paired_device):
    """A request admitted through one node reports the same status through the other."""
    device = paired_device
    node = second_node(scenario)
    await_ready(node, scenario)
    # Admit work through the first node, then read the same grant on the second.
    device.sessions(scenario.host)
    with scenario.fixture_bearer(device) as token:
        status, hosts = read_at(NODE_B, '/v1/hosts', token)
    assert status == 200
    assert scenario.host in {row['hostId'] for row in hosts['hosts']}
    request_id, kind = latest_request(scenario.schema, device.identity['device_id'])
    assert request_id
    # `kind` proves the record is the handshake/query just admitted, not an
    # unrelated earlier row. The column stores the enum number (1 or 2).
    assert kind in {'1', '2', 'HANDSHAKE', 'QUERY'}
    with scenario.fixture_bearer(device) as token:
        first_status, first_body = read_at(18080, f'/v1/requests/{request_id}', token)
        second_status, second_body = read_at(NODE_B, f'/v1/requests/{request_id}', token)
    assert first_status == second_status == 200
    assert first_body['status'] == second_body['status']


@native_dsh
def test_pairing_through_the_second_node_consumes_the_invitation_once(scenario):
    """An invitation created on one node is claimed and consumed on the other."""
    node = second_node(scenario)
    await_ready(node, scenario)
    device = Device(scenario.directory)
    invitation = http('/v1/pairing/invitations', {'hostId': scenario.host}, scenario.admin)
    uri = invitation['pairingUri'].replace('18080', str(NODE_B))
    device.identity = device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=uri)
    assert device.identity['host_id'] == scenario.host
    with scenario.fixture_bearer(device) as token:
        status, hosts = read_at(18080, '/v1/hosts', token)
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


@native_dsh
@pytest.mark.storage
@pytest.mark.relay_directory
def test_two_node_ac09_healthy_owner_upload_finish_fetch(scenario, paired_device):
    """A ciphertext object uploaded, finished and stored through node A is fetched byte-exactly through node B."""
    node = second_node(scenario)
    await_ready(node, scenario)
    payload = ciphertext(64 * 1024, 3)
    device = paired_device
    first = directory_at(18080)
    second = directory_at(NODE_B)
    with scenario.fixture_bearer(device) as token:
        status, _, body = reserve_at(
            first, token, scenario, device, payload, channel_id='channel-healthy')
        assert status == 200, body
        reservation = json.loads(body)
        assert reservation['ownerNodeId'] == NODE_A_OWNER
        status, _, body = first.upload(token, reservation, payload)
        assert status == 204, body
        status, _, body = first.finish(token, reservation['objectId'], payload)
        assert status == 200, body
        stored = json.loads(body)
        assert stored['ownerNodeId'] == NODE_A_OWNER
        assert int(stored['ciphertextBytes']) == len(payload)

        # The same owner and Device scope resolves the exact bytes on node B.
        status, headers, body = second.fetch(token, reservation['objectId'])
        assert_exact(status, headers, body, payload)

        # Another Device cannot read the same object through either node.
        other = scenario.pair_second()
        with scenario.fixture_bearer(other) as other_token:
            status, _, body = second.fetch(other_token, reservation['objectId'])
        assert status == 403, body
        assert error(body)['code'] == 'forbidden'

        # A caller naming a different node is refused, not served.
        status, headers, body = second.fetch(
            token, reservation['objectId'], owner_node='node-foreign')
        assert status == 409, body
        assert error(body)['code'] == 'conflict'
        assert error(body)['reason'] == 'wrong_owner_path'
        assert header(headers, 'WhaleRein-Owner-Node') == 'node-foreign'
        assert_refused(status, body, payload)

        # Cleanup is visible through the public delete and fetch surface.
        status, _, body = second.delete(token, reservation['objectId'])
        assert status == 200, body
        assert json.loads(body)['cancelled'] is True
        status, _, body = second.fetch(token, reservation['objectId'])
        assert status in {404, 410}, body
        assert_refused(status, body, payload)


@pytest.mark.storage
@pytest.mark.relay_directory(cleanup_interval="250ms")
def test_two_node_ac09_peer_cleanup_keeps_owner_object(scenario, paired_device):
    """A distinct-owner node running cleanup and recovery must not fence the owner's finished object."""
    node = second_node(scenario, owner=NODE_B_OWNER)
    await_ready(node, scenario)
    payload = ciphertext(8192, 19)
    device = paired_device
    first = directory_at(18080)
    second = directory_at(NODE_B)
    with scenario.fixture_bearer(device) as token:
        reservation, _ = store_at(
            first, token, scenario, device, payload, owner_node=NODE_A_OWNER,
            channel_id='channel-owner-scoped')
        object_id = reservation['objectId']

        # A non-owner fetch without the owner route is a pure miss and cannot
        # change the owner's row. Repeating it stays non-destructive.
        for _ in range(2):
            status, _, body = second.fetch(token, object_id)
            assert status == 404, body
            assert error(body)['code'] == 'not_found'
            assert_refused(status, body, payload)

        # At 250ms per tick, 4s covers at least one cleanup pass and the
        # periodic reconciliation pass that used to fence every ready row.
        time.sleep(4.0)

        status, headers, body = first.fetch(
            token, object_id, owner_node=NODE_A_OWNER)
        assert_exact(status, headers, body, payload)
        status, _, body = second.fetch(token, object_id)
        assert status == 404, body
        assert error(body)['code'] == 'not_found'
        assert_refused(status, body, payload)
        status, headers, body = first.fetch(
            token, object_id, owner_node=NODE_A_OWNER)
        assert_exact(status, headers, body, payload)

        status, _, body = first.delete(
            token, object_id, owner_node=NODE_A_OWNER)
        assert status == 200, body
        assert json.loads(body)['cancelled'] is True


@native_dsh
@pytest.mark.storage
@pytest.mark.relay_directory
def test_two_node_ac09_owner_loss_history(scenario, paired_device):
    """After the owner Access stops, another node refuses the owner route and Host history stays readable."""
    node = second_node(scenario, owner=NODE_B_OWNER)
    await_ready(node, scenario)
    payload = ciphertext(32 * 1024, 5)
    device = paired_device
    first = directory_at(18080)
    second = directory_at(NODE_B)
    with scenario.fixture_bearer(device) as token:
        reservation, _ = store_at(
            first, token, scenario, device, payload, owner_node=NODE_A_OWNER,
            channel_id='channel-owner-loss')
        object_id = reservation['objectId']
        # The healthy owner route serves the exact bytes.
        status, headers, body = first.fetch(
            token, object_id, owner_node=NODE_A_OWNER)
        assert_exact(status, headers, body, payload)
        # A different node refuses the owner route while the owner is healthy.
        status, headers, body = second.fetch(
            token, object_id, owner_node=NODE_A_OWNER)
        assert status == 409, body
        assert error(body)['code'] == 'conflict'
        assert error(body)['reason'] == 'wrong_owner_path'
        assert header(headers, 'WhaleRein-Owner-Node') == NODE_A_OWNER
        assert_refused(status, body, payload)

        # Host authority and the admitted request stay readable through B.
        device.sessions(scenario.host)
        request_id, _ = latest_request(scenario.schema, device.identity['device_id'])
        assert request_id
        status, before = read_at(NODE_B, f'/v1/requests/{request_id}', token)
        assert status == 200

        # Owner loss: stop node A and confirm B still refuses the owner route.
        scenario.access.close()
        wait(lambda: status_at(18080, '/readyz') is None, timeout=10)
        status, headers, body = second.fetch(
            token, object_id, owner_node=NODE_A_OWNER)
        assert status == 409, body
        assert error(body)['reason'] == 'wrong_owner_path'
        assert header(headers, 'WhaleRein-Owner-Node') == NODE_A_OWNER
        assert_refused(status, body, payload)
        # The original Host identity and the admitted request are still there.
        status, after = read_at(NODE_B, f'/v1/requests/{request_id}', token)
        assert status == 200
        assert after['status'] == before['status']
        status, hosts = read_at(NODE_B, '/v1/hosts', token)
        assert status == 200
        assert scenario.host in {row['hostId'] for row in hosts['hosts']}

        # A healthy object owned and served by node B still round-trips.
        local = ciphertext(4096, 9)
        local_reservation, _ = store_at(
            second, token, scenario, device, local, owner_node=NODE_B_OWNER,
            channel_id='channel-node-b')
        assert local_reservation['ownerNodeId'] == NODE_B_OWNER
        status, headers, body = second.fetch(
            token, local_reservation['objectId'], owner_node=NODE_B_OWNER)
        assert_exact(status, headers, body, local)
        status, _, body = second.delete(
            token, local_reservation['objectId'], owner_node=NODE_B_OWNER)
        assert status == 200, body
        # The lost owner's object remains fenced to that owner; B cannot delete it.
        status, _, body = second.delete(
            token, object_id, owner_node=NODE_A_OWNER)
        assert status == 409, body
        # Probing the non-owner node without the owner route is a pure miss:
        # it neither serves the object nor fences the owner's shared row.
        for _ in range(2):
            status, _, body = second.fetch(token, object_id)
            assert status == 404, body
            assert error(body)['code'] == 'not_found'
            assert_refused(status, body, payload)


@pytest.mark.storage
@pytest.mark.relay_directory
def test_two_node_ac09_attachment_part_round_trips(scenario, paired_device):
    """A 1 MiB attachment-part object crosses the two-node path with exact bytes and digest."""
    node = second_node(scenario)
    await_ready(node, scenario)
    payload = ciphertext(1024 * 1024, 7)
    assert len(payload) > 64 * 1024
    device = paired_device
    first = directory_at(18080)
    second = directory_at(NODE_B)
    with scenario.fixture_bearer(device) as token:
        reservation, stored = store_at(
            first, token, scenario, device, payload,
            channel_id='channel-attachment',
            purpose='PAYLOAD_PURPOSE_ATTACHMENT_PART',
            part_id='part-1',
        )
        assert stored['purpose'] == 'PAYLOAD_PURPOSE_ATTACHMENT_PART'
        assert int(stored['ciphertextBytes']) == len(payload)
        status, headers, body = second.fetch(token, reservation['objectId'])
        assert_exact(status, headers, body, payload)
        assert len(body) == 1024 * 1024
        status, _, body = second.delete(token, reservation['objectId'])
        assert status == 200, body
        assert json.loads(body)['cancelled'] is True
        status, _, body = second.fetch(token, reservation['objectId'])
        assert status in {404, 410}, body
        assert_refused(status, body, payload)


@pytest.mark.storage
@pytest.mark.relay_directory(
    retention="3s", upload_timeout="2s", read_timeout="5s",
    cleanup_interval="1s", max_objects=1, max_active_uploads=1,
)
def test_two_node_ac09_expired_object_is_terminal_and_reclaimed(scenario, paired_device):
    """An expired object is denied with 410, is never served by the wrong node, and cleanup frees its quota."""
    node = second_node(scenario, owner=NODE_B_OWNER)
    await_ready(node, scenario)
    payload = ciphertext(1024, 11)
    device = paired_device
    first = directory_at(18080)
    second = directory_at(NODE_B)
    with scenario.fixture_bearer(device) as token:
        expires = int((time.time() + 1) * 1000)
        reservation, _ = store_at(
            first, token, scenario, device, payload, owner_node=NODE_A_OWNER,
            channel_id='channel-expired', expires_at_ms=expires)
        object_id = reservation['objectId']
        time.sleep(2.2)
        status, _, body = first.fetch(token, object_id, owner_node=NODE_A_OWNER)
        assert status == 410, body
        assert error(body)['code'] == 'expired'
        assert_refused(status, body, payload)
        # The wrong node refuses the owner route instead of serving stale bytes.
        status, headers, body = second.fetch(
            token, object_id, owner_node=NODE_A_OWNER)
        assert status == 409, body
        assert error(body)['reason'] == 'wrong_owner_path'
        assert header(headers, 'WhaleRein-Owner-Node') == NODE_A_OWNER
        assert_refused(status, body, payload)

        # Object count is one: a new reservation is admitted only after the
        # owner's bounded cleanup released the expired charge.
        admitted = None
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            status, _, body = reserve_at(
                first, token, scenario, device, b'x' * 64,
                owner_node=NODE_A_OWNER, channel_id='channel-reclaim',
                expires_at_ms=int((time.time() + 2) * 1000))
            if status == 200:
                admitted = json.loads(body)
                break
            assert status == 429, body
            assert error(body)['reason'] == 'quota_exceeded'
            time.sleep(0.2)
        assert admitted is not None, "expired object's capacity was not reclaimed"
        status, _, body = first.delete(
            token, admitted['objectId'], owner_node=NODE_A_OWNER)
        assert status == 200, body
        assert json.loads(body)['cancelled'] is True


@pytest.mark.storage
@pytest.mark.relay_directory
def test_two_node_ac09_unadmitted_and_admitted(scenario, paired_device):
    """An unpaired Device and an unfinished object are refused; a finished object is admitted and cleanable."""
    node = second_node(scenario, owner=NODE_B_OWNER)
    await_ready(node, scenario)
    payload = ciphertext(8192, 13)
    device = paired_device
    first = directory_at(18080)
    second = directory_at(NODE_B)
    # A paired Device whose grant was withdrawn is no longer admitted.
    unpaired = scenario.pair_second()
    unpaired.revoke()
    with scenario.fixture_bearer(unpaired) as token:
        status, _, body = reserve_at(
            first, token, scenario, unpaired, payload,
            owner_node=NODE_A_OWNER, channel_id='channel-unadmitted')
    assert status in {401, 403}, body
    assert error(body)['code'] in {'unauthorized', 'forbidden'}

    with scenario.fixture_bearer(device) as token:
        # Reserved but not finished: the object is not admitted for reading.
        status, _, body = reserve_at(
            first, token, scenario, device, payload, owner_node=NODE_A_OWNER,
            channel_id='channel-pending')
        assert status == 200, body
        pending = json.loads(body)
        status, _, body = first.fetch(
            token, pending['objectId'], owner_node=NODE_A_OWNER)
        assert status == 409, body
        assert_refused(status, body, payload)
        status, _, body = second.fetch(
            token, pending['objectId'], owner_node=NODE_A_OWNER)
        assert status == 409, body
        assert error(body)['reason'] == 'wrong_owner_path'

        # Finish admits the exact object.
        status, _, body = first.upload(
            token, pending, payload, owner_node=NODE_A_OWNER)
        assert status == 204, body
        status, _, body = first.finish(
            token, pending['objectId'], payload, owner_node=NODE_A_OWNER)
        assert status == 200, body
        status, headers, body = first.fetch(
            token, pending['objectId'], owner_node=NODE_A_OWNER)
        assert_exact(status, headers, body, payload)
        # The admitted object is still refused on the wrong node.
        status, _, body = second.fetch(
            token, pending['objectId'], owner_node=NODE_A_OWNER)
        assert status == 409, body
        assert_refused(status, body, payload)

        # Delete closes admission and the object is not served again.
        status, _, body = first.delete(
            token, pending['objectId'], owner_node=NODE_A_OWNER)
        assert status == 200, body
        assert json.loads(body)['cancelled'] is True
        status, _, body = first.fetch(
            token, pending['objectId'], owner_node=NODE_A_OWNER)
        assert status in {404, 410}, body
        assert_refused(status, body, payload)


@pytest.mark.storage
@pytest.mark.relay_directory
def test_two_node_ac09_source_gone_after_reserve_and_finish(scenario, paired_device):
    """Deleting the source after finish or before upload makes the object terminal and unservable elsewhere."""
    node = second_node(scenario, owner=NODE_B_OWNER)
    await_ready(node, scenario)
    payload = ciphertext(4096, 17)
    device = paired_device
    first = directory_at(18080)
    second = directory_at(NODE_B)
    with scenario.fixture_bearer(device) as token:
        # Source gone after a complete finish.
        reservation, _ = store_at(
            first, token, scenario, device, payload, owner_node=NODE_A_OWNER,
            channel_id='channel-source-gone')
        object_id = reservation['objectId']
        status, _, body = first.delete(token, object_id, owner_node=NODE_A_OWNER)
        assert status == 200, body
        assert json.loads(body)['cancelled'] is True
        status, _, body = first.delete(token, object_id, owner_node=NODE_A_OWNER)
        assert status == 200, body
        # proto3 JSON omits a false boolean, so absence means not live.
        assert json.loads(body).get('cancelled', False) is False
        status, headers, body = first.fetch(
            token, object_id, owner_node=NODE_A_OWNER)
        assert status in {404, 410}, body
        assert error(body)['code'] in {'not_found', 'expired'}
        assert_refused(status, body, payload)
        status, _, body = second.fetch(
            token, object_id, owner_node=NODE_A_OWNER)
        assert status == 409, body
        assert error(body)['reason'] == 'wrong_owner_path'
        assert_refused(status, body, payload)

        # Source gone before upload: finish and fetch cannot resurrect it.
        status, _, body = reserve_at(
            first, token, scenario, device, payload, owner_node=NODE_A_OWNER,
            channel_id='channel-source-gone-reserved')
        assert status == 200, body
        pending = json.loads(body)
        status, _, body = first.delete(
            token, pending['objectId'], owner_node=NODE_A_OWNER)
        assert status == 200, body
        assert json.loads(body)['cancelled'] is True
        status, _, body = first.upload(
            token, pending, payload, owner_node=NODE_A_OWNER)
        assert status in {404, 409, 410}, body
        status, _, body = first.finish(
            token, pending['objectId'], payload, owner_node=NODE_A_OWNER)
        assert status in {404, 409, 410}, body
        status, _, body = first.fetch(
            token, pending['objectId'], owner_node=NODE_A_OWNER)
        assert status in {404, 409, 410}, body
        assert_refused(status, body, payload)
