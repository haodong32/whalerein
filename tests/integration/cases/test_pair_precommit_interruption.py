"""Standalone pairing interrupted before the Access grant commit."""
import os
import subprocess
import pytest
from environment import ROOT, http, wait
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__('native-dsh')
COMPANION = 'host-integration-2'


def carry_invitation(scenario, host):
    return http('/v1/pairing/invitations', {'hostId': host}, scenario.admin)['pairingUri']


def device_env(device):
    """The same environment `Device.invoke` uses, for a process we control."""
    return {
        **os.environ,
        'WHALEREIN_SECRET_BACKEND': 'file:' + str(device.directory / 'secrets'),
        'WHALEREIN_LOG': 'error',
    }


def companion(scenario):
    directory = scenario.directory / 'second-native'
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = COMPANION
    other.http_port = 3110
    other.env = scenario.env.copy()
    return other


def sql(schema, statement):
    result = subprocess.run(
        ['psql', '-h', '127.0.0.1', '-p', '55432', '-U', 'postgres',
         '-d', 'whalerein_test', '-tAc', f'SET search_path TO {schema}; {statement}'],
        check=True, capture_output=True, text=True, timeout=5)
    # `SET` echoes its own status line, so the value is the last non-empty line.
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return lines[-1] if lines else ''


def live_grants(schema, device_id):
    return int(sql(schema, f"SELECT count(*) FROM grants WHERE device_id='{device_id}' AND revoked_at IS NULL"))


def fence_key(device_id):
    return f"hashtextextended('whalerein-device:' || '{device_id}', 0)"


def hold_device_fence(schema, device_id):
    """Hold the Access device advisory fence in a dedicated session.

    The pairing commit takes `pg_advisory_xact_lock` for the same key, so this
    holder pauses the commit deterministically instead of by timing.
    """
    statement = (f'SET search_path TO {schema}; '
                 f'SELECT pg_advisory_lock({fence_key(device_id)}); '
                 f'SELECT pg_sleep(180)')
    holder = subprocess.Popen(
        ['psql', '-h', '127.0.0.1', '-p', '55432', '-U', 'postgres',
         '-d', 'whalerein_test', '-tAc', statement],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def held():
        if holder.poll() is not None:
            raise AssertionError('device fence holder exited before holding the lock')
        probe = subprocess.run(
            ['psql', '-h', '127.0.0.1', '-p', '55432', '-U', 'postgres',
             '-d', 'whalerein_test', '-tAc',
             f'SET search_path TO {schema}; SELECT pg_try_advisory_lock({fence_key(device_id)})'],
            capture_output=True, text=True, timeout=5)
        lines = [line.strip() for line in probe.stdout.splitlines() if line.strip()]
        return probe.returncode == 0 and lines[-1:] == ['f']

    try:
        wait(held, timeout=15)
    except Exception:
        release_fence(holder)
        raise
    return holder


def release_fence(holder):
    if holder.poll() is None:
        holder.kill()
    holder.communicate(timeout=10)


def test_interrupted_commit_leaves_original_grant_and_recovers(scenario, paired_device):
    """A pairing killed while its Access commit waits on the device fence commits no new grant and leaves the original grant intact."""
    schema = scenario.schema
    device = paired_device
    device_id = device.identity['device_id']
    first = scenario.create()
    other = companion(scenario)
    try:
        other.start_native()
        holder = hold_device_fence(schema, device_id)
        try:
            command = [str(ROOT / 'target/debug/whalerein'), '--json', '--yes',
                       '--data-dir', str(device.directory / 'client'),
                       'pair', carry_invitation(scenario, other.host)]
            attempt = subprocess.Popen(
                command, cwd=ROOT, env=device_env(device),
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            # The commit is parked on the fence, so no second grant can exist yet.
            wait(lambda: attempt.poll() is None, timeout=10)
            assert live_grants(schema, device_id) == 1
            attempt.kill()
            attempt.communicate(timeout=10)
        finally:
            release_fence(holder)
        # The interrupted commit must not leave a grant or a half-bound peer.
        wait(lambda: live_grants(schema, device_id) == 1, timeout=15)
        assert live_grants(schema, device_id) == 1
        assert scenario.host in {row['host_id'] for row in device.hosts()}
        assert other.host not in {row['host_id'] for row in device.hosts()}
        assert [row['session_id'] for row in device.sessions(scenario.host)] == [first]
        # The Host durably stages the interrupted attempt's peer binding, so the
        # same endpoint refuses a different ciphertext for it, even after a Host
        # restart. Recovery needs explicit operator revocation and a fresh
        # pairing, which is what the wire guide documents.
        code, envelopes = device.invoke(
            'pair', carry_invitation(scenario, other.host), success=False)
        assert code != 0 and envelopes[-1]['error']['code'] == 'forbidden'
        other.close()
        other.start_native()
        code, envelopes = device.invoke(
            'pair', carry_invitation(scenario, other.host), success=False)
        assert code != 0 and envelopes[-1]['error']['code'] == 'forbidden'
    finally:
        other.close()
