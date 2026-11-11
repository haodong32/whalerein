"""Actual DSH services and a local model, isolated inside the integration image."""

import hashlib

import pytest
from environment import wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def events(values):
    return [item.get("data", {}).get("event", {}) for item in values]


def text_fields(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "text" and isinstance(child, str):
                yield child
            else:
                yield from text_fields(child)
    elif isinstance(value, list):
        for child in value:
            yield from text_fields(child)


def test_native_pair_send_revoke(scenario, paired_device):
    """When an isolated actual DSH Session receives a CLI prompt, it settles one native reply and revocation denies the old transport credential."""
    session = scenario.create()
    assert any(
        item["session_id"] == session for item in paired_device.sessions(scenario.host)
    )
    scenario.model.plan()
    paired_device.send(session, "native integration fixture")
    scenario.wait_idle(session)
    assert (
        scenario.request("events", sessionId=session)["types"].count(
            "assistant/message"
        )
        == 1
    )
    paired_device.revoke()
    assert paired_device.revoked_access_denied()


def test_native_stream_prefix_and_fixed_cut(scenario, paired_device):
    """When an actual native Attempt streams a large message and later history grows, intact preview prefixes match the final message and fixed-cut paging excludes the later turn."""
    session = scenario.create()
    scenario.model.plan("large", hold=True)
    tail = scenario.tail(paired_device, session)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15), "Native model request did not start"
    wait(lambda: scenario.preview_ready(tail), timeout=8, process=tail)
    scenario.model.release.set()
    values = events(scenario.finish_tail(tail))
    final = [
        item
        for item in values
        if item.get("type") == "message" and item.get("role") == "ASSISTANT"
    ]
    assert len({item["message_id"] for item in final}) == 1, (
        "Native final identity duplicated"
    )
    body = max(text_fields(final), key=len)
    accumulated = ""
    intact = False
    for item in values:
        if item.get("type") != "preview":
            continue
        accumulated = item["text"] if item["replace"] else accumulated + item["text"]
        if not item["incomplete"]:
            intact = True
            prefix_matches = body.startswith(accumulated)
            assert prefix_matches, "Native preview prefix diverged"
    final_size, preview_size = len(body), len(accumulated)
    # Live previews are ephemeral; settlement may supersede an undelivered suffix.
    # Every intact observed prefix must match, while final delivery is complete.
    assert intact and final_size == 80015 and 0 < preview_size <= final_size
    # Golden digest for the deterministic 80 KB fixture; no payload in failures.
    final_digest = hashlib.sha256(body.encode()).hexdigest()
    assert (
        final_digest
        == "a2f5e1b62155cd7b3d1a2736527c8ac8273767af6a0985291a605681868e163c"
    )
    before = scenario.cached(paired_device, session)
    page = scenario.request("cut-start", sessionId=session)
    cut = page["cut"]
    scenario.model.plan()
    paired_device.send(session, "native later-turn fixture")
    scenario.wait_idle(session, assistant_count=2)
    for _ in range(200):
        if page["complete"]:
            break
        page = scenario.request("cut-next", sessionId=session)
    assert page["complete"] and page["cut"] == cut
    assert tuple(page["ids"]) == before[1], (
        "Frozen cut changed native message identities"
    )


def test_native_cold_restart(scenario, paired_device):
    """When the actual DSH process restarts after an idle turn, cold history preserves cursor/message identity and query recovery starts no new model request."""
    session = scenario.create()
    scenario.model.plan()
    paired_device.send(session, "native restart fixture")
    scenario.wait_idle(session)
    paired_device.sync(session)
    before = scenario.cached(paired_device, session)
    calls = scenario.model.calls
    scenario.native.close()
    scenario.start_native()
    paired_device.sync(session)
    assert scenario.cached(paired_device, session) == before
    assert scenario.model.calls == calls
    scenario.model.plan()
    paired_device.send(session, "native post-restart continuation fixture")
    scenario.wait_idle(session, assistant_count=2)
    paired_device.sync(session)
    after = scenario.cached(paired_device, session)
    assert after[0] != before[0] and after[1][: len(before[1])] == before[1]
    assert len(after[1]) == len(set(after[1])) > len(before[1])
    scenario.access.close()
    scenario.start_access()
    assert any(
        item["session_id"] == session for item in paired_device.sessions(scenario.host)
    )


@pytest.mark.parametrize(
    "decision,expected,effects",
    [
        pytest.param(
            "approve",
            "allowed-once",
            1,
            marks=pytest.mark.description(
                "When remote claim approves an actual native permission question, one decision grants exactly one fixture effect and a repeated approval cannot execute it again."
            ),
        ),
        pytest.param(
            "reject",
            "rejected",
            0,
            marks=pytest.mark.description(
                "When remote claim rejects an actual native permission question, one rejection settles the turn without executing the fixture effect."
            ),
        ),
    ],
)
def test_native_approval(scenario, paired_device, decision, expected, effects):
    """Native decision and exact fixture effect count agree."""
    session = scenario.create()
    scenario.model.plan("tool")
    paired_device.send(session, "native approval fixture")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    approval = paired_device.approvals(session)[0]
    paired_device.resolve(decision, approval)
    scenario.wait_idle(session)
    assert scenario.request("events", sessionId=session)["decisions"] == [expected]
    assert scenario.request("effects")["count"] == effects
    code, _ = paired_device.invoke(
        "approve",
        "--request",
        approval["approval_id"],
        "--digest",
        approval["digest"],
        "--version",
        str(approval["version"]),
        success=False,
    )
    assert code != 0 and scenario.request("effects")["count"] == effects


def test_native_interrupt_cancels_approval(scenario, paired_device):
    """When interruption cancels an actual native approval, a late remote approval is rejected and the protected fixture effect never executes."""
    session = scenario.create()
    scenario.model.plan("tool")
    paired_device.send(session, "native cancellation fixture")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    approval = paired_device.approvals(session)[0]
    paired_device.interrupt(session)
    wait(
        lambda: not scenario.request("state", sessionId=session)["running"],
        process=scenario.native,
    )
    code, _ = paired_device.invoke(
        "approve",
        "--request",
        approval["approval_id"],
        "--digest",
        approval["digest"],
        "--version",
        str(approval["version"]),
        success=False,
    )
    assert code != 0 and scenario.request("effects")["count"] == 0
    assert scenario.request("events", sessionId=session)["decisions"] == ["cancelled"]


def test_native_browser_settings_persist(scenario):
    """When the real DSH browser opens WhaleRein settings, credentials stay masked, connection testing authenticates, and saved fields survive a page reload."""
    scenario.browser(
        "settings", hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"]
    )


def test_native_browser_bad_draft_is_read_only(scenario):
    """When a browser connection draft uses a rejected credential, the real host stays online and reload discards the unsaved secret."""
    scenario.browser(
        "bad-draft", hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"]
    )
    assert scenario.request("ready")["connected"]


def test_native_browser_invitation_and_revoke(scenario):
    """When the real settings page generates an invitation and revokes its paired CLI, pairing succeeds and the old transport credential is rejected afterward."""
    from environment import Device

    invitation = scenario.directory / "browser-invitation.txt"
    scenario.browser(
        "invite",
        invitationFile=str(invitation),
        hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
    )
    device = Device(scenario.directory)
    device.identity = device.call("pair", invitation.read_text())
    device._revoked_token = device.transport_token()
    scenario.browser(
        "revoke", hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"]
    )
    assert device.revoked_access_denied()


def test_native_browser_claim_ownership(scenario, paired_device):
    """When a real browser observes the native Session before a claimed question starts, only remote approval owns it and exactly one native decision/effect settles it."""
    scenario.browser_claim(paired_device)


def test_native_interrupt_stream(scenario, paired_device):
    """When the actual native model stream is interrupted before its held completion, the turn stops and read-only recovery starts no replacement model call."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    paired_device.send(session, "native stream cancellation fixture")
    assert scenario.model.started.wait(timeout=15)
    paired_device.interrupt(session)
    wait(
        lambda: not scenario.request("state", sessionId=session)["running"],
        process=scenario.native,
    )
    calls = scenario.model.calls
    scenario.model.release.set()
    paired_device.sync(session)
    assert scenario.model.calls == calls == 1


def test_native_first_pair_after_access_restart(scenario):
    """When Access restarts before the first pairing, the native host recovers framed transport capability without a previously paired client priming it."""
    scenario.access.close()
    scenario.start_access()
    device = scenario.pair()
    session = scenario.create()
    assert any(item["session_id"] == session for item in device.sessions(scenario.host))
