"""Cross-process revocation preserves only opaque unconfirmed command identities."""
import json
import os
import subprocess
from pathlib import Path

import pytest
from environment import ROOT, wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def retained_request_ids(device):
    directory = device.data_dir / "unconfirmed-commands"
    return sorted(entry.stem for entry in directory.glob("*.json"))


def revocation_marker(device):
    path = device.data_dir / "admission-revoked"
    return path.read_text().strip() if path.exists() else ""


def test_native_revocation_preserves_admitted_command_across_cli_processes(scenario, paired_device):
    """A separately revoked profile retains an admitted native command for manual verification."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    assert scenario.request("arm-channel-queue-barrier", config={"phase":"post-native"})["armed"]
    args=paired_device.args("session", 'queue', "remove", "--session", session, "--item", item)
    process=subprocess.Popen(args, cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(lambda: scenario.request("channel-queue-barrier")["entered"], timeout=15, process=scenario.native)
        pending=paired_device.call("operation", "list")["pending_commands"]
        assert len(pending) == 1
        identity=pending[0]["request_id"]
        assert paired_device.invoke("device", "revoke", "--yes", success=False)[0] == 0
        listing=paired_device.call("operation", "list")
        assert listing["pending_commands"] == []
        assert retained_request_ids(paired_device) == [identity]
        assert not (paired_device.data_dir / "cache.sqlite3").exists()
        data=(paired_device.data_dir / "unconfirmed-commands" / (identity + ".json")).read_text()
        assert pending[0]["command_id"] not in data and pending[0]["host_id"] not in data
        scenario.request("release-channel-queue-barrier")
        process.communicate(timeout=25)
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == []
        assert retained_request_ids(paired_device) == [identity]
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            scenario.request("release-channel-queue-barrier")
            process.kill()
            process.communicate(timeout=5)


def test_native_confirmed_revocation_survives_sigkill_until_explicit_forget(scenario, paired_device):
    """Confirmed revocation fences later commands until the user explicitly forgets local state."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    code, _ = paired_device.invoke("session", "queue", "remove", "--session", session, "--item", item, success=False)
    assert code != 0
    pending=paired_device.call("operation", "list")["pending_commands"]
    assert len(pending) == 1
    barrier=paired_device.directory / "revoke-paused"
    env=paired_device.env(WHALEREIN_TEST_REVOCATION_BARRIER=str(barrier))
    process=subprocess.Popen(paired_device.args("--yes", 'device', 'revoke'), cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(barrier.exists, timeout=20)
        assert (paired_device.data_dir / "admission-revoked").exists()
        process.kill()
        process.communicate(timeout=5)
        assert process.returncode < 0
        path = paired_device.data_dir
        marker = revocation_marker(paired_device)
        assert marker.startswith("confirmed:")
        regional = (path / "regional.json").read_bytes()
        assert (path / "cache.sqlite3").is_file()
        assert not paired_device.has_secret("device-token")
        assert not paired_device.has_secret("cache-key")

        assert paired_device.call("status")["device_access"] == "revoked"
        code, values = paired_device.invoke("operation", "list", success=False)
        assert code == 4 and values[-1]["error"]["code"] == "revoked"
        assert "unable to verify" in values[-1]["error"]["message"]
        assert revocation_marker(paired_device) == marker
        assert (path / "regional.json").read_bytes() == regional
        assert (path / "cache.sqlite3").is_file()
        assert retained_request_ids(paired_device) == []

        forgotten = paired_device.call("device", "forget", "--local-only", "--yes")
        assert forgotten["identity_forgotten"] is True
        assert forgotten["remote_grant_may_remain"] is True
        assert paired_device.call("operation", "list")["pending_commands"] == []
        assert not (path / "cache.sqlite3").exists()
        assert not (path / "regional.json").exists()
        assert not paired_device.has_secret("device-identity")
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == []
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_native_revocation_fences_waiting_admission_across_processes(scenario, paired_device):
    """A process paused before cache admission cannot submit after another process revokes."""
    import fcntl
    import signal
    import time
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    path=paired_device.data_dir
    env=paired_device.env()
    ready=paired_device.directory / "admission-waiting"
    env["WHALEREIN_TEST_CACHE_INIT_BARRIER"]=str(ready)
    lock=(path / "admission.lock").open("a+")
    fcntl.flock(lock, fcntl.LOCK_EX)
    process=subprocess.Popen(paired_device.args("session", 'queue', "remove", "--session", session, "--item", item),cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    stopped=False
    try:
        wait(ready.exists, timeout=15)
        os.kill(process.pid, signal.SIGSTOP)
        stopped=True
        fcntl.flock(lock, fcntl.LOCK_UN)
        assert paired_device.invoke("device", "revoke", "--yes", success=False)[0] == 0
        ready.unlink()
        os.kill(process.pid, signal.SIGCONT)
        stopped=False
        output,_=process.communicate(timeout=20)
        assert process.returncode == 4
        # Revocation removed the token while the old process retained its
        # regional snapshot. Dispatch stops when it tries to open Access.
        assert json.loads(output)["error"]["code"] == "not_paired"
        assert paired_device.call("status")["device_access"] == "revoked"
        assert scenario.request("queue-native-ids",sessionId=session)["ids"] == [item]
        listing=paired_device.call("operation", "list")
        assert listing["pending_commands"] == [] and retained_request_ids(paired_device) == []
    finally:
        if stopped:
            os.kill(process.pid, signal.SIGCONT)
        fcntl.flock(lock, fcntl.LOCK_UN)
        lock.close()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)
        scenario.model.release.set()


def test_native_sigkill_after_remote_revoke_resumes_pending(scenario, paired_device):
    """A kill after Access deletion leaves pending state and retry completes cleanup."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add",sessionId=session)
    assert scenario.request("queue-read",sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("session", "queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("operation", "list")["pending_commands"][0]["request_id"]
    barrier=paired_device.directory / "after-delete"
    env=paired_device.env(WHALEREIN_TEST_DELETE_CONFIRMED_BARRIER=str(barrier))
    process=subprocess.Popen(paired_device.args("--yes", 'device', 'revoke'),cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        process.kill()
        process.communicate(timeout=5)
        listing=paired_device.call("operation", "list")
        assert revocation_marker(paired_device).startswith("pending:")
        assert listing["pending_commands"][0]["request_id"] == identity
        code, result=paired_device.invoke("device", "revoke", "--yes", success=False)
        assert code != 0 and result[-1]["error"]["code"] == "revoked"
        assert paired_device.call("operation", "list")["pending_commands"] == []
        assert not revocation_marker(paired_device).startswith("pending:")
        assert retained_request_ids(paired_device) == [identity]
        assert not (paired_device.data_dir / "cache.sqlite3").exists()
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_native_sigkill_after_repair_commit_keeps_new_grant(scenario, paired_device):
    """A killed re-pair after its config commit never replays old revocation onto the new grant."""
    from environment import http
    assert paired_device.invoke("device", "revoke", "--yes", success=False)[0] == 0
    invitation=http("/v1/pairing/invitations",{"hostId":scenario.host},scenario.admin)
    barrier=paired_device.directory / "after-repair"
    env=paired_device.env(WHALEREIN_TEST_PAIR_COMMITTED_BARRIER=str(barrier))
    process=subprocess.Popen(paired_device.args("--yes", "host", "connect", "--invite-stdin"),cwd=ROOT,env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    process.stdin.write(invitation["pairingUri"])
    process.stdin.close()
    process.stdin = None
    try:
        wait(barrier.exists,timeout=20)
        process.kill()
        process.communicate(timeout=5)
        assert not (paired_device.data_dir / "admission-revoked").exists()
        assert paired_device.call("host", "list")["hosts"]
        assert (paired_device.data_dir / "regional.json").exists()
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_native_sigkill_after_observed_revoke_resumes_check(scenario, paired_device):
    """A killed observer retains pending state and a later check completes global revoke cleanup."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add",sessionId=session)
    assert scenario.request("queue-read",sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("session", "queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("operation", "list")["pending_commands"][0]["request_id"]
    import urllib.request
    request=urllib.request.Request("http://127.0.0.1:18080/v1/devices/" + paired_device.identity["device_id"], method="DELETE", headers={"Authorization":"Bearer " + scenario.admin})
    with urllib.request.urlopen(request, timeout=5) as response:
        assert response.status in (200, 204)
    barrier=paired_device.directory / "observed-revoke"
    env=paired_device.env(WHALEREIN_TEST_OBSERVED_REVOKED_BARRIER=str(barrier))
    process=subprocess.Popen(paired_device.args("session", "list", "--host", scenario.host),cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        process.kill()
        process.communicate(timeout=5)
        pending=paired_device.call("operation", "list")
        assert revocation_marker(paired_device).startswith("pending:")
        assert pending["pending_commands"][0]["request_id"] == identity
        code,result=paired_device.invoke("session", "list", "--host", scenario.host, success=False)
        assert code != 0 and result[-1]["error"]["code"] == "revoked"
        assert retained_request_ids(paired_device) == [identity]
        assert not (paired_device.data_dir / "cache.sqlite3").exists()
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_native_concurrent_revokers_share_intent_if_second_is_killed(scenario, paired_device):
    """A successful revoker still cleans when a concurrent revoker is killed before DELETE."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add",sessionId=session)
    assert scenario.request("queue-read",sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("session", "queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("operation", "list")["pending_commands"][0]["request_id"]
    path=paired_device.data_dir
    after=paired_device.directory / "after-delete-a"
    before=paired_device.directory / "before-delete-b"
    args=paired_device.args("--yes", 'device', 'revoke')
    common=paired_device.env()
    first=subprocess.Popen(args,cwd=ROOT,env={**common,"WHALEREIN_TEST_DELETE_CONFIRMED_BARRIER":str(after)},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    second=None
    try:
        wait(after.exists,timeout=20)
        second=subprocess.Popen(args,cwd=ROOT,env={**common,"WHALEREIN_TEST_BEFORE_DELETE_BARRIER":str(before)},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        wait(before.exists,timeout=20)
        second.kill()
        second.communicate(timeout=5)
        after.unlink()
        first.communicate(timeout=25)
        assert first.returncode == 0
        assert paired_device.call("operation", "list")["pending_commands"] == []
        assert retained_request_ids(paired_device) == [identity]
        assert not (path / "cache.sqlite3").exists()
    finally:
        scenario.model.release.set()
        for process in (first,second):
            if process is not None and process.poll() is None:
                process.kill()
                process.communicate(timeout=5)


def test_native_observer_cannot_be_displaced_by_concurrent_revoker(scenario, paired_device):
    """A revoker cannot replace an observer that already confirmed external authorization loss."""
    import urllib.request
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add",sessionId=session)
    assert scenario.request("queue-read",sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("session", "queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("operation", "list")["pending_commands"][0]["request_id"]
    request=urllib.request.Request("http://127.0.0.1:18080/v1/devices/"+paired_device.identity["device_id"],method="DELETE",headers={"Authorization":"Bearer "+scenario.admin})
    with urllib.request.urlopen(request,timeout=5) as response:
        assert response.status in (200,204)
    path=paired_device.data_dir
    barrier=paired_device.directory / "observer-held"
    env=paired_device.env(WHALEREIN_TEST_OBSERVED_REVOKED_BARRIER=str(barrier))
    observer=subprocess.Popen(paired_device.args("session", "list", "--host", scenario.host),cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        before=(path / "admission-revoked").read_text()
        code,result=paired_device.invoke("device", "revoke", "--yes", success=False)
        assert code != 0 and result[-1]["error"]["code"] == "unavailable"
        assert (path / "admission-revoked").read_text() == before
        barrier.unlink()
        observer.communicate(timeout=25)
        assert observer.returncode != 0
        assert retained_request_ids(paired_device) == [identity]
        assert not (path / "cache.sqlite3").exists()
    finally:
        scenario.model.release.set()
        if observer.poll() is None:
            observer.kill()
            observer.communicate(timeout=5)


def test_native_read_only_startup_does_not_clear_new_grant(scenario, paired_device):
    """Opening status preserves a newer pairing while its revocation is pending."""
    from environment import http

    paired_device.revoke()
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    paired_device.pair(invitation["pairingUri"])
    path = paired_device.data_dir
    barrier = paired_device.directory / "new-revoke"
    process = subprocess.Popen(
        paired_device.args("device", "revoke", "--yes"),
        cwd=ROOT,
        env=paired_device.env(WHALEREIN_TEST_BEFORE_DELETE_BARRIER=str(barrier)),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        wait(barrier.exists, timeout=20)
        before = revocation_marker(paired_device)
        config = (path / "regional.json").read_bytes()
        assert paired_device.has_secret("device-token")
        outcome = paired_device.call("status")
        assert outcome["device_access"] == "revocation_pending"
        assert (path / "regional.json").read_bytes() == config
        assert revocation_marker(paired_device) == before
        assert paired_device.has_secret("device-token")
    finally:
        barrier.unlink(missing_ok=True)
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_native_snapshot_inode_replacement_is_reported_and_not_deleted(scenario, paired_device):
    """Replacing the cache after its snapshot is detected without deleting the replacement."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add",sessionId=session)
    assert scenario.request("queue-read",sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("session", "queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("operation", "list")["pending_commands"][0]["request_id"]
    path=paired_device.data_dir
    barrier=paired_device.directory / "snapshot-before-unlink"
    env=paired_device.env(WHALEREIN_TEST_SNAPSHOT_BEFORE_UNLINK=str(barrier))
    process=subprocess.Popen(paired_device.args("--yes", 'device', 'revoke'),cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        (path / "cache.sqlite3").rename(path / "displaced-cache")
        (path / "cache.sqlite3").write_bytes(b"synthetic replacement")
        barrier.unlink()
        process.communicate(timeout=25)
        assert process.returncode != 0
        assert (path / "cache.sqlite3").read_bytes() == b"synthetic replacement"
        assert paired_device.call("operation", "list")["pending_commands"] == []
        assert retained_request_ids(paired_device) == [identity]
        assert revocation_marker(paired_device).split(":")[-1] in {"check", "waiting", "lost", "conflict"} or (paired_device.data_dir / "revocation-retention-warning").exists()
        assert revocation_marker(paired_device).endswith(":conflict")
        assert (path / "cache.sqlite3").read_bytes() == b"synthetic replacement"
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


@pytest.mark.parametrize("completion", [
    pytest.param("commit", marks=pytest.mark.description("A transient cache writer commits and the revoked identity is retained.")),
    pytest.param("rollback", marks=pytest.mark.description("A transient cache writer rolls back and the revoked identity is retained.")),
])
def test_native_transient_cache_writer_retains_identity(scenario, paired_device, completion):
    """A revoked profile waits for a transient writer after clearing credentials."""
    import sys
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add",sessionId=session)
    assert scenario.request("queue-read",sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("session", "queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("session", "queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("operation", "list")["pending_commands"][0]["request_id"]
    path=paired_device.data_dir
    script="""import sqlite3,sys
c=sqlite3.connect(sys.argv[1],timeout=1)
c.execute('BEGIN IMMEDIATE')
c.execute('UPDATE commands SET updated_at_unix_ms=updated_at_unix_ms+1')
print('ready',flush=True)
action=sys.stdin.readline().strip()
c.commit() if action=='commit' else c.rollback()
c.close()
"""
    writer=subprocess.Popen([sys.executable,"-u","-c",script,str(path / "cache.sqlite3")],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    revoker=None
    try:
        assert writer.stdout.readline().strip() == "ready"
        env=paired_device.env()
        revoker=subprocess.Popen(paired_device.args("--yes", 'device', 'revoke'),cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        wait(
            lambda: revocation_marker(paired_device).endswith(":waiting")
            and not paired_device.has_secret("device-token")
            and not paired_device.has_secret("cache-key"),
            timeout=15,
        )
        assert revoker.poll() is None and (path / "cache.sqlite3").exists()
        writer.stdin.write(completion+"\n");writer.stdin.flush()
        writer.communicate(timeout=5)
        assert writer.returncode == 0
        output,_=revoker.communicate(timeout=20)
        result=json.loads(output)
        assert revoker.returncode == 0 and result["data"]["revoked_retention_unverified"] is False
        assert retained_request_ids(paired_device) == [identity]
    finally:
        scenario.model.release.set()
        for process in (revoker,writer):
            if process is not None and process.poll() is None:
                process.kill();process.communicate(timeout=5)


def test_native_revocation_after_host_directory_read_fences_control(scenario, paired_device):
    """A directory read cannot authorize a later control after the grant is revoked."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item = paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    try:
        assert any(host["host_id"] == scenario.host for host in paired_device.hosts())
        paired_device.revoke()
        assert paired_device.cli_access_denied()
        assert not (paired_device.data_dir / "cache.sqlite3").exists()
        code, values = paired_device.invoke(
            "session", "queue", "remove", "--session", session, "--item", item, success=False
        )
        assert code != 0
        assert values[-1]["error"]["code"] in {"revoked", "not_paired"}
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == [item]
    finally:
        scenario.model.release.set()


def test_native_repair_keeps_the_revoked_command_unrecoverable(scenario, paired_device):
    """A repaired profile still lists the revoked command identity, cannot recover it, and does not run it again."""
    from environment import http

    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item = paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    assert scenario.request("arm-channel-queue-barrier", config={"phase": "post-native"})["armed"]
    args = paired_device.args("session", 'queue', "remove", "--session", session, "--item", item)
    process = subprocess.Popen(args, cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(lambda: scenario.request("channel-queue-barrier")["entered"], timeout=15, process=scenario.native)
        pending = paired_device.call("operation", "list")["pending_commands"]
        assert len(pending) == 1
        identity = pending[0]["request_id"]
        assert paired_device.invoke("device", "revoke", "--yes", success=False)[0] == 0
        scenario.request("release-channel-queue-barrier")
        process.communicate(timeout=25)
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == []
        invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
        repaired = paired_device.invoke("host", "connect", "--invite-stdin", "--yes", stdin_text=invitation["pairingUri"], success=False)
        assert repaired[0] == 0
        # A repair keeps the device identity, which is why the opaque entry is
        # still attributable to this profile for manual review.
        assert repaired[1][-1]["data"]["device_id"] == paired_device.identity["device_id"]
        assert retained_request_ids(paired_device) == [identity]
        code, values = paired_device.invoke("operation", "recover", identity, success=False)
        assert code == 3 and values[-1]["error"]["code"] == "not_found"
        assert values[-1].get("data") is None
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == []
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            scenario.request("release-channel-queue-barrier")
            process.kill()
            process.communicate(timeout=5)
