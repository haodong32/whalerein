"""Native DSH operator journeys (JRNY-02, JRNY-07, JRNY-12).

These cases need the actual pinned DSH runtime, its real Approval and Session
services, and the real settings page in Chromium. They add no product logic:
the deterministic local model and the fixture tool only make the native timing
observable.
"""

import time

import pytest

from environment import wait

from journey_helpers import (
    approval_args,
    assistant_texts,
    browser_journey,
    continue_process,
    host_online,
    stop_process,
)

pytestmark = pytest.mark.__getattr__("native-dsh")


def _event(value):
    return value.get("data", {}).get("event") or {}


def _texts(event):
    return [block.get("text", "") for block in event.get("blocks", [])]


# --- JRNY-02 ----------------------------------------------------------------


@pytest.mark.description(
    "A client suspended longer than its transport keepalive rebuilds the native watch on resume and shows the reply committed during the suspension exactly once."
)
def test_journey_suspended_client_resumes_native_stream(scenario, paired_device):
    """A background suspension behaves like a warm resume, not a cold start."""
    session = scenario.create()
    watch = paired_device.start_watch("tail", "--session", session, "--timeout", "90")
    try:
        wait(lambda: scenario.request("watches") > 0, process=scenario.native)
        scenario.model.plan(hold=True)
        paired_device.send(session, "native suspend fixture", delivery="start-if-idle")
        assert scenario.model.started.wait(timeout=15)

        # The phone is backgrounded: the CLI process is frozen mid-stream.
        stop_process(watch.process)
        try:
            scenario.model.release.set()
            scenario.wait_idle(session)
            # Longer than the 10 second device keepalive, so the broker dropped
            # the half-open session while the client was suspended.
            time.sleep(17)
        finally:
            continue_process(watch.process)

        watch.wait(
            lambda value: _event(value).get("type") == "message"
            and _event(value).get("role") == "ASSISTANT"
            and any("native fixture response" in text for text in _texts(_event(value))),
            timeout=45,
        )
        time.sleep(0.3)
        reply = "native fixture response"
        assert assistant_texts(watch.values).count(reply) == 1

        # The resumed watch still receives new native turns.
        scenario.model.plan()
        paired_device.send(session, "native resume second fixture", delivery="start-if-idle")
        watch.wait(
            lambda value: _event(value).get("type") == "message"
            and _event(value).get("role") == "ASSISTANT"
            and any("native fixture response" in text for text in _texts(_event(value))),
            timeout=30,
        )
    finally:
        watch.close()


# --- JRNY-07 ----------------------------------------------------------------


@pytest.mark.description(
    "When one operator interrupts a native task while a second operator watches and holds the approval card, the second operator sees an idle session, a rejected approval and no source effect."
)
def test_journey_interrupt_while_another_operator_holds_the_approval(
    scenario, paired_device
):
    """Cross-operator interruption cannot leave the second view running and pending."""
    session = scenario.create()
    second = scenario.pair_second()
    scenario.model.plan("tool")
    paired_device.send(session, "cross operator interrupt fixture", delivery="start-if-idle")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    card = second.approvals(session)[0]
    view = second.start_watch("tail", "--session", session, "--timeout", "60")
    try:
        wait(lambda: scenario.request("watches") > 0, process=scenario.native)
        paired_device.interrupt(session)
        wait(
            lambda: not second.approvals(session),
            timeout=20,
            process=scenario.native,
        )
        observation = second.call("wait", "--session", session, "--timeout", "3")
        assert observation["reason"] == "idle"
        assert observation["state"]["pending_input"] == []

        code, values = second.invoke("approve", *approval_args(card), success=False)
        assert code != 0 and values[-1].get("data") is None
        assert scenario.request("effects")["count"] == 0
        assert scenario.request("events", sessionId=session)["decisions"] == ["cancelled"]

        # The watching operator's stream is still usable for the next turn.
        scenario.model.plan()
        paired_device.send(
            session, "cross operator recovery fixture", delivery="start-if-idle"
        )
        view.wait(
            lambda value: _event(value).get("type") == "message"
            and _event(value).get("role") == "ASSISTANT"
            and any("native fixture response" in text for text in _texts(_event(value))),
            timeout=30,
        )
    finally:
        view.close()


# --- JRNY-12 ----------------------------------------------------------------


@pytest.mark.description(
    "A lost device is identified from the Host settings device list and revoked there, while the surviving device keeps its grant and can send a native turn."
)
def test_journey_lost_device_revoked_from_settings(scenario, paired_device):
    """The settings device list is the recovery source when a device is lost."""
    first = paired_device
    lost = scenario.pair_second()
    lost._revoked_token = lost.transport_token()
    lost_id = lost.identity["device_id"]
    first_id = first.identity["device_id"]
    assert lost_id != first_id

    reply = browser_journey(
        scenario,
        "revoke-device",
        hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
        revokeDeviceId=lost_id,
    )
    observed = " ".join(reply["devices"])
    assert lost_id in observed and first_id in observed
    assert len(reply["remaining"]) == 1
    assert all(lost_id not in row for row in reply["remaining"])

    assert lost.revoked_access_denied()
    assert host_online(first, scenario.host) is True
    session = scenario.create()
    sent = first.send(session, "survivor after revoke fixture", delivery="start-if-idle")
    assert sent["status"] == "succeeded"
    scenario.wait_idle(session)
