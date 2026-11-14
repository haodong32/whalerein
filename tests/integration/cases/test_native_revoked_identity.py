"""Cross-process revocation preserves only opaque unconfirmed command identities."""
import json
import os
import subprocess
from pathlib import Path

import pytest
from environment import ROOT, wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_native_revocation_preserves_admitted_command_across_cli_processes(scenario, paired_device):
    """A separately revoked profile retains an admitted native command for manual verification."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("queue", "list", "--session", session)["items"][0]["item_id"]
    assert scenario.request("arm-channel-queue-barrier", config={"phase":"post-native"})["armed"]
    args=[str(ROOT / "target/debug/whalerein"), "--json", "--yes", "--data-dir", str(paired_device.directory / "client"), "queue", "remove", "--session", session, "--item", item]
    process=subprocess.Popen(args, cwd=ROOT, env={**os.environ, "WHALEREIN_SECRET_BACKEND":"file:" + str(paired_device.directory / "secrets"), "WHALEREIN_LOG":"error"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(lambda: scenario.request("channel-queue-barrier")["entered"], timeout=15, process=scenario.native)
        pending=paired_device.call("commands-pending")["commands"]
        assert len(pending) == 1
        identity=pending[0]["request_id"]
        assert paired_device.invoke("revoke", success=False)[0] == 0
        listing=paired_device.call("commands-pending")
        assert listing["commands"] == []
        assert listing["unconfirmed_after_revocation"] == [identity]
        assert not (paired_device.directory / "client/cache.sqlite3").exists()
        data=(paired_device.directory / "client/unconfirmed-commands" / (identity + ".json")).read_text()
        assert pending[0]["command_id"] not in data and pending[0]["host_id"] not in data
        scenario.request("release-channel-queue-barrier")
        process.communicate(timeout=25)
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == []
        assert paired_device.call("commands-pending")["unconfirmed_after_revocation"] == [identity]
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            scenario.request("release-channel-queue-barrier")
            process.kill()
            process.communicate(timeout=5)


def test_native_sigkill_revocation_resumes_and_cleans(scenario, paired_device):
    """A killed revoker is cleaned up and reports unverified retention without the original connection."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item=paired_device.call("queue", "list", "--session", session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    code, _ = paired_device.invoke("queue", "remove", "--session", session, "--item", item, success=False)
    assert code != 0
    pending=paired_device.call("commands-pending")["commands"]
    assert len(pending) == 1
    barrier=paired_device.directory / "revoke-paused"
    env={**os.environ, "WHALEREIN_SECRET_BACKEND":"file:" + str(paired_device.directory / "secrets"), "WHALEREIN_LOG":"error", "WHALEREIN_TEST_REVOCATION_BARRIER":str(barrier)}
    process=subprocess.Popen([str(ROOT / "target/debug/whalerein"), "--json", "--yes", "--data-dir", str(paired_device.directory / "client"), "revoke"], cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(barrier.exists, timeout=20)
        assert (paired_device.directory / "client/admission-revoked").exists()
        process.kill()
        process.communicate(timeout=5)
        assert process.returncode < 0
        code,values=paired_device.invoke("commands-pending",success=False)
        assert code != 0 and "unable to verify" in values[-1]["error"]["message"]
        listing=paired_device.call("commands-pending")
        assert listing["commands"] == []
        assert listing["unconfirmed_after_revocation"] == []
        assert listing["revoked_retention_unverified"] is True
        assert not (paired_device.directory / "client/cache.sqlite3").exists()
        assert not (paired_device.directory / "client/config.json").exists()
        device_id=paired_device.identity["device_id"]
        for kind in ("device-token", "cache-key"):
            key=(f"whalerein:v1:{kind}:{device_id}").encode().hex()
            assert not (paired_device.directory / "secrets" / key).exists()
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
    item=paired_device.call("queue", "list", "--session", session)["items"][0]["item_id"]
    path=paired_device.directory / "client"
    env={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error"}
    ready=paired_device.directory / "admission-waiting"
    env["WHALEREIN_TEST_CACHE_INIT_BARRIER"]=str(ready)
    lock=(path / "admission.lock").open("a+")
    fcntl.flock(lock, fcntl.LOCK_EX)
    process=subprocess.Popen([str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(path),"queue","remove","--session",session,"--item",item],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    stopped=False
    try:
        wait(ready.exists, timeout=15)
        os.kill(process.pid, signal.SIGSTOP)
        stopped=True
        fcntl.flock(lock, fcntl.LOCK_UN)
        assert paired_device.invoke("revoke",success=False)[0] == 0
        ready.unlink()
        os.kill(process.pid, signal.SIGCONT)
        stopped=False
        output,_=process.communicate(timeout=20)
        assert process.returncode != 0
        assert json.loads(output)["error"]["code"] == "revoked"
        assert scenario.request("queue-native-ids",sessionId=session)["ids"] == [item]
        listing=paired_device.call("commands-pending")
        assert listing["commands"] == [] and listing["unconfirmed_after_revocation"] == []
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
    item=paired_device.call("queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("commands-pending")["commands"][0]["request_id"]
    barrier=paired_device.directory / "after-delete"
    env={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error","WHALEREIN_TEST_DELETE_CONFIRMED_BARRIER":str(barrier)}
    process=subprocess.Popen([str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(paired_device.directory / "client"),"revoke"],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        process.kill()
        process.communicate(timeout=5)
        listing=paired_device.call("commands-pending")
        assert listing["authorization_verification_pending"] is True
        assert listing["commands"][0]["request_id"] == identity
        code, result=paired_device.invoke("revoke",success=False)
        assert code != 0 and result[-1]["error"]["code"] == "revoked"
        final=paired_device.call("commands-pending")
        assert final["authorization_verification_pending"] is False
        assert final["unconfirmed_after_revocation"] == [identity]
        assert not (paired_device.directory / "client/cache.sqlite3").exists()
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_native_sigkill_after_repair_commit_keeps_new_grant(scenario, paired_device):
    """A killed re-pair after its config commit never replays old revocation onto the new grant."""
    from environment import http
    assert paired_device.invoke("revoke",success=False)[0] == 0
    invitation=http("/v1/pairing/invitations",{"hostId":scenario.host},scenario.admin)
    barrier=paired_device.directory / "after-repair"
    env={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error","WHALEREIN_TEST_PAIR_COMMITTED_BARRIER":str(barrier)}
    process=subprocess.Popen([str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(paired_device.directory / "client"),"pair",invitation["pairingUri"]],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        process.kill()
        process.communicate(timeout=5)
        assert not (paired_device.directory / "client/admission-revoked").exists()
        assert paired_device.call("hosts")["hosts"]
        assert (paired_device.directory / "client/config.json").exists()
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
    item=paired_device.call("queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("commands-pending")["commands"][0]["request_id"]
    import urllib.request
    request=urllib.request.Request("http://127.0.0.1:18080/v1/devices/" + paired_device.identity["device_id"], method="DELETE", headers={"Authorization":"Bearer " + scenario.admin})
    with urllib.request.urlopen(request, timeout=5) as response:
        assert response.status in (200, 204)
    barrier=paired_device.directory / "observed-revoke"
    env={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error","WHALEREIN_TEST_OBSERVED_REVOKED_BARRIER":str(barrier)}
    process=subprocess.Popen([str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(paired_device.directory / "client"),"hosts"],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        process.kill()
        process.communicate(timeout=5)
        pending=paired_device.call("commands-pending")
        assert pending["authorization_verification_pending"] is True
        assert pending["commands"][0]["request_id"] == identity
        code,result=paired_device.invoke("hosts",success=False)
        assert code != 0 and result[-1]["error"]["code"] == "revoked"
        assert paired_device.call("commands-pending")["unconfirmed_after_revocation"] == [identity]
        assert not (paired_device.directory / "client/cache.sqlite3").exists()
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
    item=paired_device.call("queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("commands-pending")["commands"][0]["request_id"]
    path=paired_device.directory / "client"
    after=paired_device.directory / "after-delete-a"
    before=paired_device.directory / "before-delete-b"
    args=[str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(path),"revoke"]
    common={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error"}
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
        result=paired_device.call("commands-pending")
        assert result["unconfirmed_after_revocation"] == [identity]
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
    item=paired_device.call("queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("commands-pending")["commands"][0]["request_id"]
    request=urllib.request.Request("http://127.0.0.1:18080/v1/devices/"+paired_device.identity["device_id"],method="DELETE",headers={"Authorization":"Bearer "+scenario.admin})
    with urllib.request.urlopen(request,timeout=5) as response:
        assert response.status in (200,204)
    path=paired_device.directory / "client"
    barrier=paired_device.directory / "observer-held"
    env={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error","WHALEREIN_TEST_OBSERVED_REVOKED_BARRIER":str(barrier)}
    observer=subprocess.Popen([str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(path),"hosts"],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        before=(path / "admission-revoked").read_text()
        code,result=paired_device.invoke("revoke",success=False)
        assert code != 0 and result[-1]["error"]["code"] == "unavailable"
        assert (path / "admission-revoked").read_text() == before
        barrier.unlink()
        observer.communicate(timeout=25)
        assert observer.returncode != 0
        assert paired_device.call("commands-pending")["unconfirmed_after_revocation"] == [identity]
        assert not (path / "cache.sqlite3").exists()
    finally:
        scenario.model.release.set()
        if observer.poll() is None:
            observer.kill()
            observer.communicate(timeout=5)


def test_native_stale_startup_fence_does_not_clear_new_grant(scenario, paired_device):
    """A stale restarting process cannot clean a newer pairing and its revocation intent."""
    from environment import http
    assert paired_device.invoke("revoke",success=False)[0] == 0
    path=paired_device.directory / "client"
    old_barrier=paired_device.directory / "old-startup"
    new_barrier=paired_device.directory / "new-revoke"
    common={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error"}
    args=[str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(path)]
    stale=subprocess.Popen(args+["commands-pending"],cwd=ROOT,env={**common,"WHALEREIN_TEST_STARTUP_FENCE_BARRIER":str(old_barrier)},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    newer=None
    try:
        wait(old_barrier.exists,timeout=20)
        invitation=http("/v1/pairing/invitations",{"hostId":scenario.host},scenario.admin)
        assert paired_device.invoke("pair",invitation["pairingUri"],success=False)[0] == 0
        newer=subprocess.Popen(args+["revoke"],cwd=ROOT,env={**common,"WHALEREIN_TEST_BEFORE_DELETE_BARRIER":str(new_barrier)},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        wait(new_barrier.exists,timeout=20)
        expected=(path / "admission-revoked").read_text()
        old_barrier.unlink()
        stale.communicate(timeout=20)
        assert stale.returncode == 0
        assert (path / "config.json").exists()
        key=("whalerein:v1:device-token:"+paired_device.identity["device_id"]).encode().hex()
        assert (paired_device.directory / "secrets" / key).exists()
        assert (path / "admission-revoked").read_text() == expected
    finally:
        for process in (stale,newer):
            if process is not None and process.poll() is None:
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
    item=paired_device.call("queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("commands-pending")["commands"][0]["request_id"]
    path=paired_device.directory / "client"
    barrier=paired_device.directory / "snapshot-before-unlink"
    env={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error","WHALEREIN_TEST_SNAPSHOT_BEFORE_UNLINK":str(barrier)}
    process=subprocess.Popen([str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(path),"revoke"],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    try:
        wait(barrier.exists,timeout=20)
        (path / "cache.sqlite3").rename(path / "displaced-cache")
        (path / "cache.sqlite3").write_bytes(b"synthetic replacement")
        barrier.unlink()
        process.communicate(timeout=25)
        assert process.returncode != 0
        assert (path / "cache.sqlite3").read_bytes() == b"synthetic replacement"
        listing=paired_device.call("commands-pending")
        assert listing["unconfirmed_after_revocation"] == [identity]
        assert listing["revoked_retention_unverified"] is True
        assert listing["revocation_cleanup_requires_inspection"] is True
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
    item=paired_device.call("queue","list","--session",session)["items"][0]["item_id"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    assert paired_device.invoke("queue","remove","--session",session,"--item",item,success=False)[0] != 0
    identity=paired_device.call("commands-pending")["commands"][0]["request_id"]
    path=paired_device.directory / "client"
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
        env={**os.environ,"WHALEREIN_SECRET_BACKEND":"file:"+str(paired_device.directory / "secrets"),"WHALEREIN_LOG":"error"}
        revoker=subprocess.Popen([str(ROOT / "target/debug/whalerein"),"--json","--yes","--data-dir",str(path),"revoke"],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        token=("whalerein:v1:device-token:"+paired_device.identity["device_id"]).encode().hex()
        key=("whalerein:v1:cache-key:"+paired_device.identity["device_id"]).encode().hex()
        wait(lambda: (path / "admission-revoked").exists() and (path / "admission-revoked").read_text().endswith(":waiting") and not (paired_device.directory / "secrets" / token).exists() and not (paired_device.directory / "secrets" / key).exists(), timeout=15)
        assert revoker.poll() is None and (path / "cache.sqlite3").exists()
        writer.stdin.write(completion+"\n");writer.stdin.flush()
        writer.communicate(timeout=5)
        assert writer.returncode == 0
        output,_=revoker.communicate(timeout=20)
        result=json.loads(output)
        assert revoker.returncode == 0 and result["data"]["revoked_retention_unverified"] is False
        assert paired_device.call("commands-pending")["unconfirmed_after_revocation"] == [identity]
    finally:
        scenario.model.release.set()
        for process in (revoker,writer):
            if process is not None and process.poll() is None:
                process.kill();process.communicate(timeout=5)


def test_native_revocation_after_host_cache_admission_fences_result_and_control(scenario, paired_device):
    """A successful Host lookup cannot hide revocation before its cache update."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item = paired_device.call("queue", "list", "--session", session)["items"][0]["item_id"]
    barrier = paired_device.directory / "host-sync-barrier"
    env = {**os.environ, "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"), "WHALEREIN_LOG": "error", "WHALEREIN_TEST_HOST_SYNC_BARRIER": str(barrier)}
    process = subprocess.Popen([str(ROOT / "target/debug/whalerein"), "--json", "--yes", "--data-dir", str(paired_device.directory / "client"), "hosts"], cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(barrier.exists, timeout=20)
        assert paired_device.invoke("revoke", success=False)[0] == 0
        barrier.unlink()
        output, _ = process.communicate(timeout=20)
        assert process.returncode != 0
        assert json.loads(output)["error"]["code"] == "revoked"
        assert paired_device.invoke("queue", "remove", "--session", session, "--item", item, success=False)[0] != 0
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == [item]
    finally:
        barrier.unlink(missing_ok=True)
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)
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
    item = paired_device.call("queue", "list", "--session", session)["items"][0]["item_id"]
    assert scenario.request("arm-channel-queue-barrier", config={"phase": "post-native"})["armed"]
    args = [str(ROOT / "target/debug/whalerein"), "--json", "--yes", "--data-dir", str(paired_device.directory / "client"), "queue", "remove", "--session", session, "--item", item]
    process = subprocess.Popen(args, cwd=ROOT, env={**os.environ, "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"), "WHALEREIN_LOG": "error"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(lambda: scenario.request("channel-queue-barrier")["entered"], timeout=15, process=scenario.native)
        pending = paired_device.call("commands-pending")["commands"]
        assert len(pending) == 1
        identity = pending[0]["request_id"]
        assert paired_device.invoke("revoke", success=False)[0] == 0
        scenario.request("release-channel-queue-barrier")
        process.communicate(timeout=25)
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == []
        invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
        repaired = paired_device.invoke("pair", invitation["pairingUri"], success=False)
        assert repaired[0] == 0
        # A repair keeps the device identity, which is why the opaque entry is
        # still attributable to this profile for manual review.
        assert repaired[1][-1]["data"]["device_id"] == paired_device.identity["device_id"]
        assert paired_device.call("commands-pending")["unconfirmed_after_revocation"] == [identity]
        code, values = paired_device.invoke("command-recover", "--request", identity, success=False)
        assert code == 3 and values[-1]["error"]["code"] == "not_found"
        assert values[-1].get("data") is None
        assert scenario.request("queue-native-ids", sessionId=session)["ids"] == []
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            scenario.request("release-channel-queue-barrier")
            process.kill()
            process.communicate(timeout=5)
