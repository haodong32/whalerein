"""QMQ question lifecycle: phases, pagination cursors, content budgets and reads."""

import json

import pytest

from environment import wait

pytestmark = pytest.mark.chain


def _answer_args(session, batch, phase=None):
    return (
        "--session", session,
        "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"],
        "--version", str(batch["version"]),
        "--digest", batch["digest"],
        "--phase", phase or batch["phase"],
    )


@pytest.mark.description("An unanswerable or mistyped question phase is invalid_argument, not a stale conflict.")
def test_question_phase_errors_are_input_errors(scenario, paired_device, session_id):
    scenario.dsh.process.control("request_question_unanswerable", session_id=session_id)
    batch = paired_device.call("session", "question", "list", "--session", session_id)["batches"][0]
    assert batch["phase"] == "QUESTION_REPLY_QUEUED"
    assert batch["answerable"] is False
    answer = scenario.directory / "qmq-phase-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": []}]}))

    # The same (unanswerable) phase is rejected as an input problem.
    code, values = paired_device.invoke(
        "session", "question", "answer", *_answer_args(session_id, batch), "--input", str(answer), success=False
    )
    assert code == 2
    assert values[-1]["error"]["code"] == "invalid_argument"
    assert "not answerable" in values[-1]["error"]["message"]

    # A misspelled phase is also an input problem, never a stale-data conflict.
    code, values = paired_device.invoke(
        "session", "question", "answer", *_answer_args(session_id, batch, "QUESTION_OPNE"), "--input", str(answer), success=False
    )
    assert code == 2
    assert values[-1]["error"]["code"] == "invalid_argument"
    assert "unknown question phase" in values[-1]["error"]["message"]

    # A valid requested phase against an unanswerable source phase stays an
    # input error instead of telling the user the question changed.
    code, values = paired_device.invoke(
        "session", "question", "answer", *_answer_args(session_id, batch, "QUESTION_CONTINUED"), "--input", str(answer), success=False
    )
    assert code == 2
    assert values[-1]["error"]["code"] == "invalid_argument"
    assert "current question phase" in values[-1]["error"]["message"]


@pytest.mark.description("Question content with Unicode, newlines and tabs round-trips without loss.")
def test_question_unicode_content_round_trips(scenario, paired_device, session_id):
    text = "第一行\n第二行\t😀 café  "
    assert scenario.dsh.process.control("request_question_custom", session_id=session_id, text=text)
    batches = paired_device.call("session", "question", "list", "--session", session_id)["batches"]
    assert len(batches) == 1
    assert batches[0]["questions"][0]["question"] == text
    # The exact UTF-8 content is what the source holds; no truncation happened.
    assert len(text.encode("utf-8")) > len(text)


@pytest.mark.description("Adding or answering a batch mid-pagination never turns a valid cursor into a conflict.")
def test_question_pagination_survives_source_changes(scenario, paired_device, session_id):
    scenario.dsh.process.control("request_question_pages", session_id=session_id)
    first = paired_device.call("session", "question", "list", "--session", session_id)
    assert len(first["batches"]) == 1 and first["next_cursor"]
    cursor = first["next_cursor"]

    # A new batch appears after the delivered anchor.
    scenario.dsh.process.control("append_question_page", session_id=session_id, text="third")
    second = paired_device.call("session", "question", "list", "--session", session_id, "--cursor", cursor)
    assert len(second["batches"]) == 1 and second["next_cursor"]
    third = paired_device.call("session", "question", "list", "--session", session_id, "--cursor", second["next_cursor"])
    assert len(third["batches"]) == 1 and not third["next_cursor"]

    # The delivered anchor is answered/removed: the recorded successor resumes.
    scenario.dsh.process.control("remove_question_page", session_id=session_id, text="first")
    resumed = paired_device.call("session", "question", "list", "--session", session_id, "--cursor", cursor)
    assert len(resumed["batches"]) == 1
    assert resumed["batches"][0]["phase"] == "QUESTION_CONTINUED"
    assert resumed["next_cursor"]


@pytest.mark.description("A question pagination cursor continues paging across a Host restart.")
def test_question_pagination_cursor_survives_plugin_restart(scenario, paired_device, session_id):
    scenario.dsh.process.control("request_question_pages", session_id=session_id)
    first = paired_device.call("session", "question", "list", "--session", session_id)
    assert first["next_cursor"]
    # The pager key is persisted in the Host state store; a restarted plugin
    # must authenticate the old cursor instead of reporting a conflict.
    scenario.plugin.close()
    scenario.start_plugin()
    second = paired_device.call("session", "question", "list", "--session", session_id, "--cursor", first["next_cursor"])
    assert len(second["batches"]) == 1
    assert second["batches"][0]["batch_id"] != first["batches"][0]["batch_id"]


@pytest.mark.description("An online Host without question support exits 9 while an offline Host exits 5.")
def test_question_read_distinguishes_unsupported_from_offline(scenario, paired_device, session_id):
    paired_device.sessions(scenario.host)
    assert scenario.dsh.process.control("set_questions_enabled", session_id=session_id, text=False)
    code, values = paired_device.invoke("session", "question", "list", "--session", session_id, success=False)
    assert code == 9
    assert values[-1]["error"]["code"] == "unsupported_capability"
    # Restoring support proves the previous failure was the capability, not a
    # connectivity problem.
    assert scenario.dsh.process.control("set_questions_enabled", session_id=session_id, text=True)
    assert paired_device.call("session", "question", "list", "--session", session_id)["batches"] == []
    # An offline Host is a separate, retryable class.
    scenario.plugin.close()
    code, values = paired_device.invoke("session", "question", "list", "--session", session_id, success=False)
    assert code == 5
    assert values[-1]["error"]["code"] == "host_offline"


@pytest.mark.description("A mistyped question-cancel command id points at real pending commands instead of a missing request.")
def test_question_cancel_typo_guidance_is_actionable(scenario, paired_device, session_id):
    paired_device.sessions(scenario.host)
    code, values = paired_device.invoke(
        "session", "question", "cancel", "--session", session_id, "--command", "qmq-typo-command", success=False
    )
    assert code == 0
    assert values[-1]["data"]["cancellation_requested"] is False
    # The human guidance is a separate channel from JSON; exercise it without --json.
    from environment import ROOT  # local import keeps the case module light
    import os
    import subprocess

    result = subprocess.run(
        [str(ROOT / "target/debug/whalerein"), "--config", str(paired_device.config_path),
         "session", "question", "cancel", "--session", session_id, "--command", "qmq-typo-command"],
        cwd=ROOT, env=paired_device.env(), capture_output=True, text=True, timeout=60, check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "No pending command matches" in result.stdout
    assert "operation list" in result.stdout
    assert "operation recover" not in result.stdout
