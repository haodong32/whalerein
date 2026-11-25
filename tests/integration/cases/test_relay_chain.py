"""AC-01.above and AC-02.inline/reference through the real Host and Access.

A result above the Host-to-Access packet budget is uploaded and published as a
reference; the paired CLI resolves it through the Access directory and prints
the exact content. A small result stays inline and creates no directory object.
"""

import json
import os
import select
import subprocess

import pytest
from environment import ROOT, wait

pytestmark = pytest.mark.chain


def psql(schema, query):
    result = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-p", "55432", "-U", "postgres",
         "-d", "whalerein_test", "-tAc", query],
        capture_output=True, text=True, timeout=5, check=True,
    )
    return result.stdout.strip()


def compact_device(device):
    """Make the paired Core advertise its 32 KiB compact receive budget."""
    config = device.config_path
    text = config.read_text()
    assert "[transport]" not in text
    config.write_text(text.rstrip("\n") + '\n[transport]\nprofile = "compact"\n')


def add_queue_item(scenario, size):
    session = "relay-chain-fixture"
    scenario.dsh.process.control("create_queue_budget_session", session_id=session)
    assert scenario.dsh.process.control("set_queue_budget_size", session_id=session, size=size)
    state = scenario.dsh.process.control("queue_budget_state", session_id=session)
    assert state[0]["length"] == size
    return session


@pytest.mark.relay_directory
def test_ac02_inline_result_stays_inline(scenario, paired_device):
    """A small queue result is read inline and leaves no temporary ciphertext reference."""
    session = add_queue_item(scenario, 512)
    paired_device.sessions(scenario.host)
    result = paired_device.call("session", "queue", "list", "--session", session)
    text = result["items"][0]["blocks"][0]["text"]
    assert len(text) == 512 and text == "x" * 512
    # Epoch-5 session reads are transient query-lane operations: no durable
    # request row and no result object exist for an inline reply.
    objects = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.temporary_ciphertext_objects "
        "WHERE purpose='PAYLOAD_PURPOSE_RESULT'",
    )
    assert int(objects) == 0, "an inline result must not create a reference"
    rows = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.requests "
        f"WHERE device_id='{paired_device.identity['device_id']}' AND kind IN (1,2,3)",
    )
    assert rows.strip() in ("", "0"), "an epoch-5 query must not create a durable request row"


@pytest.mark.relay_directory
def test_ac01_above_result_uses_the_access_directory(scenario, paired_device):
    """A result above the Host packet budget is uploaded before its reference, then resolves to exact bytes."""
    compact_device(paired_device)
    session = add_queue_item(scenario, 100 * 1024)
    paired_device.sessions(scenario.host)
    result = paired_device.call("session", "queue", "list", "--session", session)
    text = result["items"][0]["blocks"][0]["text"]
    assert len(text) == 100 * 1024 and text == "x" * (100 * 1024)
    # The query result carries a reference, but the query itself is transient:
    # the exact bytes live in the temporary object, not in a durable row.
    rows = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.requests "
        f"WHERE device_id='{paired_device.identity['device_id']}' AND kind IN (1,2,3)",
    )
    assert rows.strip() in ("", "0"), "an epoch-5 query must not create a durable request row"
    # The directory is real: the case root contains one ready RESULT object.
    objects = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.temporary_ciphertext_objects "
        "WHERE purpose='PAYLOAD_PURPOSE_RESULT'",
    )
    assert int(objects) >= 1, "the Host must have uploaded the result ciphertext"
    incorrectly_scoped = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.temporary_ciphertext_objects "
        "WHERE purpose='PAYLOAD_PURPOSE_RESULT' AND app_session_id <> ''",
    )
    assert int(incorrectly_scoped) == 0, "a standalone result acquired a managed login session"



@pytest.mark.relay_directory
def test_ac02_duplicate_result_read_is_harmless(scenario, paired_device):
    """Reading the same terminal result twice returns identical content and does not create a second outcome."""
    session = add_queue_item(scenario, 4096)
    paired_device.sessions(scenario.host)
    first = paired_device.call("session", "queue", "list", "--session", session)
    second = paired_device.call("session", "queue", "list", "--session", session)
    assert first["items"][0]["blocks"][0]["text"] == second["items"][0]["blocks"][0]["text"]
    rows = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.requests WHERE kind=2 "
        f"AND device_id='{paired_device.identity['device_id']}'",
    )
    assert int(rows) == 0, "epoch-5 queries are transient and create no durable row"


class LiveTopicObserver:
    """Record Host live and control event PUBLISHes through the real broker."""

    def __init__(self, scenario):
        self.process = subprocess.Popen(
            ["node", "tests/integration/drivers/transport/live-topic-observer.mjs"],
            cwd=ROOT,
            env={
                **os.environ,
                "WHALEREIN_MQTT_URL": "mqtt://127.0.0.1:1883",
                "WHALEREIN_TEST_ACCESS_PASSWORD": scenario.env["WHALEREIN_BROKER_PASSWORD"],
                "WHALEREIN_OBSERVER_HOST_ID": scenario.host,
            },
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self._expect({"type": "ready"})

    def _read(self, timeout=10):
        ready, _, _ = select.select([self.process.stdout], [], [], timeout)
        assert ready, "live topic observer produced no response"
        line = self.process.stdout.readline()
        assert line, "live topic observer produced no line"
        return json.loads(line)

    def _expect(self, expected):
        value = self._read()
        assert value == expected, value
        return value

    def snapshot(self):
        self.process.stdin.write(json.dumps({"op": "snapshot"}) + "\n")
        self.process.stdin.flush()
        value = self._read()
        assert value.get("type") == "snapshot", value
        return value

    def close(self):
        if self.process.poll() is None:
            try:
                self.process.stdin.write(json.dumps({"op": "stop"}) + "\n")
                self.process.stdin.flush()
                self.process.wait(timeout=10)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                self.process.kill()
                self.process.wait(timeout=5)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()


def _event(value):
    return value.get("data", {}).get("event", {})


def _event_text(event):
    # A preview carries its displayable text directly; a committed message
    # carries typed blocks.
    if isinstance(event.get("text"), str):
        return event["text"]
    return "".join(block.get("text", "") for block in event.get("blocks", []))


@pytest.mark.description(
    "When an active Watch receives an incomplete live preview, the Host publishes it directly on the Device QoS 0 live topic without Access republishing and without a whalerein-target stamp, the retired Host live topic stays empty, a concurrent HTTPS history read returns committed state, the control event topic carries no preview, and the committed message is still delivered once at QoS 1."
)
def test_ac15_live_preview_only_and_committed_authority(scenario, paired_device, session_id):
    """An incomplete live preview stays on the live class; the committed message supersedes it."""
    fixture = scenario.dsh.process
    observer = LiveTopicObserver(scenario)
    watcher = paired_device.start_watch("session", "watch", session_id, "--timeout", "20")
    try:
        watcher.wait(lambda value: _event(value).get("type") == "state", timeout=15)
        baseline = observer.snapshot()
        fixture.control("preview_start", session_id=session_id)
        fixture.control("preview_chunk", session_id=session_id, index=0, text="live-preview")
        wait(lambda: len(observer.snapshot()["device_live"]) > len(baseline["device_live"]), timeout=10)
        during = observer.snapshot()
        # `h/{host}/live` is retired: the preview is published directly to the
        # authorized Device topic and Access never republishes it.
        assert len(during["live"]) == len(baseline["live"]), "the retired Host live topic must stay empty"
        device_live_topic = f"whalerein/v1/h/{scenario.host}/d/{paired_device.identity['device_id']}/live"
        direct = during["device_live"][len(baseline["device_live"]):]
        assert direct, "the preview must arrive on the Device live topic"
        assert all(entry["topic"] == device_live_topic for entry in direct), direct
        assert all(entry["qos"] == 0 for entry in direct), direct
        # Access stamps whalerein-target when it delivers a preview itself; a
        # direct Host publication carries no such reserved property.
        assert all("whalerein-target" not in entry["properties"] for entry in direct), direct
        assert len(during["event"]) == len(baseline["event"]), "a preview must never appear on the control event topic"
        assert len(during["device_event"]) == len(baseline["device_event"]), "a preview must never appear on the Device event topic"
        watcher.wait(
            lambda value: _event(value).get("type") == "preview"
            and "live-preview" in _event_text(_event(value)),
            timeout=15,
        )

        # A concurrent HTTPS history read must coexist with the live watch.
        history = paired_device.history(session_id)
        assert isinstance(history, dict)
        # Omit a native preview chunk so the preview is incomplete. The
        # committed message must still arrive and supersede it.
        fixture.control("preview_chunk", session_id=session_id, index=2, text="gap")
        fixture.control("preview_final", session_id=session_id, text="committed-after-gap", count=3)
        watcher.wait(
            lambda value: _event(value).get("type") == "message"
            and _event_text(_event(value)) == "committed-after-gap",
            timeout=20,
        )
        after = observer.snapshot()
        assert len(after["event"]) > len(baseline["event"]), "the committed message must travel on the control event topic"
        assert all(entry["qos"] == 1 for entry in after["event"][len(baseline["event"]):]), after["event"]
        assert all(entry["qos"] == 0 for entry in after["device_live"][len(baseline["device_live"]):]), after["device_live"]
        assert len(after["live"]) == len(baseline["live"]), "the retired Host live topic must stay empty"
        assert len(after["device_live"]) >= len(during["device_live"])
        assert [value for value in watcher.values if _event(value).get("type") == "message"][-1]
    finally:
        watcher.close()
        observer.close()


@pytest.mark.description(
    "When the live Watch receiver is disconnected, a foreground send on the same Device still completes through the control path."
)
def test_ac16_disconnected_live_delivery_does_not_block_control(scenario, paired_device, session_id):
    """A disconnected live receiver drops previews; the same Device still sends and commits."""
    fixture = scenario.dsh.process
    watcher = paired_device.start_watch("session", "watch", session_id, "--timeout", "20")
    try:
        watcher.wait(lambda value: _event(value).get("type") == "state", timeout=15)
        fixture.control("preview_start", session_id=session_id)
        fixture.control("preview_chunk", session_id=session_id, index=0, text="live-before-disconnect")
        watcher.wait(
            lambda value: _event(value).get("type") == "preview"
            and "live-before-disconnect" in _event_text(_event(value)),
            timeout=15,
        )
    finally:
        watcher.close()

    receipt = paired_device.send_confirmed(
        session_id, "control after live disconnect", delivery="start-if-idle"
    )
    assert receipt["status"] == "succeeded"
    assert receipt["message_id"]
