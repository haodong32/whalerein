"""Native delivery unconfirmed receipt recovery over the encrypted path."""

import pytest

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.mark.parametrize("kind", [
    pytest.param("missing", marks=pytest.mark.description(
        "A missing native delivery receipt remains unknown across restart."
    )),
    pytest.param("negative", marks=pytest.mark.description(
        "A negative native delivery receipt remains unknown across restart."
    )),
    pytest.param("empty", marks=pytest.mark.description(
        "An empty native delivery receipt remains unknown across restart."
    )),
])
def test_native_delivery_unconfirmed_receipt_stays_unknown_after_restart(scenario, paired_device, kind):
    """After native entry, an unconfirmed return cannot create a successful receipt or cause replay."""
    import sqlite3

    session = scenario.create()
    paired_device.sessions(scenario.host)
    baseline = scenario.request("gx-user-delivery-count", sessionId=session)["count"]
    scenario.model.plan()
    assert scenario.request("arm-native-unconfirmed-delivery", config={"kind": kind})["armed"]
    text = "synthetic delivery"
    code, values = paired_device.invoke(
        "session", "send", "--detach", session, "--delivery", "start-if-idle", text, success=False
    )
    code, outcome = paired_device.settle_detached_delivery(
        code, values[-1], session, text, "start-if-idle"
    )
    assert code != 0 and outcome["error"]["code"] == "unknown"
    scenario.wait_idle(session)
    assert scenario.model.calls == 1
    assert scenario.request("gx-user-delivery-count", sessionId=session)["count"] == baseline + 1
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        row = db.execute("SELECT command_id, status FROM commands ORDER BY created_at DESC LIMIT 1").fetchone()
    assert row and row[1] == "unknown"
    scenario.native.close()
    scenario.start_native()
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        assert db.execute("SELECT status FROM commands WHERE command_id=?", (row[0],)).fetchone() == ("unknown",)
    status, replay = scenario.replay_command(paired_device)
    assert status in (200, 202) and replay["commandId"] == row[0]
    assert scenario.request("gx-user-delivery-count", sessionId=session)["count"] == baseline + 1
    assert scenario.model.calls == 1
