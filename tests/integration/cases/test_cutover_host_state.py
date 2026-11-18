"""Coordinated version-one cutover against retained Host state.

The old artifact is stopped first and the current artifact starts against the
same Host state directory. These cases prove the retained Host identity, peer
pin, question pager key, and command journal survive; a cursor issued by an
older pager key is reported as snapshot_required instead of being silently
reinterpreted; and a settled answer is not invoked again by the new artifact.
"""

from __future__ import annotations

import base64
import json
import sqlite3

import pytest

from environment import wait

pytestmark = pytest.mark.chain


def _state(scenario):
    """Read the retained Host state directly; no content leaves this helper."""
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite", timeout=10) as db:
        identity = db.execute(
            "SELECT public_key, fingerprint FROM host_identity"
        ).fetchone()
        peers = {
            row[0]: row[1]
            for row in db.execute("SELECT device_id, fingerprint FROM peer_keys")
        }
        meta = dict(db.execute("SELECT key, value FROM meta"))
        commands = {
            row[0]: row[1:]
            for row in db.execute(
                "SELECT command_id, status, hex(digest) FROM commands"
            )
        }
    return {
        "identity": identity,
        "peers": peers,
        "meta": meta,
        "commands": commands,
        "schema_version": meta.get("schema_version"),
    }


def _old_key_generation_cursor() -> str:
    """A cursor minted by the previous Host pager-key generation.

    The authenticated cursor format is unchanged, but its key id cannot match
    the retained key. The contract requires snapshot_required (restart from the
    first page), never a conflict or an implicit reinterpretation.
    """
    body = {
        "v": 2,
        "kid": "previous-pager-key-generation",
        "anchor": "",
        "next_id": "",
        "index": 0,
    }
    encoded = base64.urlsafe_b64encode(
        json.dumps(body, separators=(",", ":")).encode()
    ).decode().rstrip("=")
    return f"{encoded}.previous-signature"


@pytest.mark.description(
    "When the old Host artifact stops and the current artifact starts against retained state, host identity, peer pins, question pager key and Source session identity survive without re-pairing."
)
def test_cutover_retains_host_identity_pins_and_pager_key(
    scenario, paired_device, session_id
):
    scenario.dsh.process.control("request_question_pages", session_id=session_id)
    first = paired_device.call("session", "question", "list", "--session", session_id)
    assert first["next_cursor"]
    retained_cursor = first["next_cursor"]

    before = _state(scenario)
    assert before["identity"] and before["identity"][1]
    assert paired_device.identity["device_id"] in before["peers"]
    assert before["meta"].get("question_cursor_key")

    # An event queued for the old observation/channel is not durable
    # observation state. The new artifact must discard it instead of
    # delivering it into a newly opened observation.
    old_channel = "previous-observation-channel"
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite", timeout=10) as db:
        db.execute(
            "INSERT INTO event_outbox (device_id, channel_id, payload, created_at)"
            " VALUES (?,?,?,?)",
            (
                paired_device.identity["device_id"],
                old_channel,
                b"old-queued-observation",
                1,
            ),
        )
        db.commit()

    # Stop the old artifact, then start the new contract against the same
    # Host state directory and the same Source session.
    scenario.plugin.close()
    scenario.start_plugin()

    def old_channel_outbox():
        with sqlite3.connect(scenario.directory / "plugin/state.sqlite", timeout=10) as db:
            return db.execute(
                "SELECT COUNT(*) FROM event_outbox WHERE channel_id=?", (old_channel,)
            ).fetchone()[0]

    wait(lambda: old_channel_outbox() == 0, timeout=30)

    after = _state(scenario)
    assert after["identity"] == before["identity"], "Host identity was rekeyed"
    assert after["peers"] == before["peers"], "peer pin changed across cutover"
    assert (
        after["meta"].get("question_cursor_key")
        == before["meta"]["question_cursor_key"]
    ), "question pager key was regenerated"

    sessions = paired_device.sessions(scenario.host)
    assert any(
        entry.get("session_id") == session_id for entry in sessions
    ), "Source session identity did not survive the cutover"

    # The retained pager key authenticates the cursor issued before the cutover.
    second = paired_device.call(
        "session", "question", "list", "--session", session_id, "--cursor", retained_cursor
    )
    assert second["batches"]
    assert second["batches"][0]["session_id"] == session_id

    # A cursor from an older key generation is only reported as a snapshot
    # restart; it never becomes a conflict or a silently re-based page.
    code, values = paired_device.invoke(
        "session", "question", "list",
        "--session",
        session_id,
        "--cursor",
        _old_key_generation_cursor(),
        success=False,
    )
    assert code == 8
    assert values[-1]["error"]["code"] == "snapshot_required"


@pytest.mark.description(
    "When a settled answer is retained through the cutover, the new Host artifact recovers its original command identity without invoking the native source again."
)
def test_cutover_does_not_replay_a_settled_answer(
    scenario, paired_device, session_id
):
    scenario.dsh.process.control("request_question_open", session_id=session_id)
    batch = paired_device.call("session", "question", "list", "--session", session_id)["batches"][0]
    answer = scenario.directory / "cutover-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    receipt = paired_device.call(
        "session", "question", "answer",
        "--session",
        session_id,
        "--batch",
        batch["batch_id"],
        "--epoch",
        batch["source_epoch"],
        "--version",
        str(batch["version"]),
        "--digest",
        batch["digest"],
        "--phase",
        batch["phase"],
        "--input",
        str(answer),
    )
    command_id = receipt["command_id"]
    assert scenario.dsh.process.control("question_answer_calls") == 1

    before = _state(scenario)
    assert command_id in before["commands"]
    retained = before["commands"][command_id]

    scenario.plugin.close()
    scenario.start_plugin()

    after = _state(scenario)
    assert after["commands"].get(command_id) == retained, (
        "the retained command journal row changed across the cutover"
    )

    # The retained journal row is the original recovery identity, keyed by the
    # same device/command pair. Nothing in the new artifact invokes the answer
    # again merely because the process restarted.
    assert command_id in after["commands"]
    assert scenario.dsh.process.control("question_answer_calls") == 1
