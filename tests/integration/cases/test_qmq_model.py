"""QMQ model lifecycle: no-op reselection, session isolation, observed model and retry guidance."""

import pytest

from environment import wait

pytestmark = pytest.mark.chain


def _session(scenario, name):
    return scenario.dsh.process.control("create_session", session_id=name)


def _model(scenario, device, session):
    return device.call("session", "model", "get", "--session", session)


@pytest.mark.description("Re-selecting the effective next model sends no second indistinguishable command.")
def test_model_reselection_is_a_noop(scenario, paired_device):
    session = "qmq-model-noop"
    _session(scenario, session)
    paired_device.sessions(scenario.host)
    before = scenario.dsh.process.control("model_counts")["selectSessionModel"]
    first = paired_device.call("session", "model", "set", "--session", session, "--provider", "fixture-provider", "--model", "fixture-model-2")
    assert first["model_selection"]["selected"]["model"] == "fixture-model-2"
    assert scenario.dsh.process.control("model_counts")["selectSessionModel"] == before + 1
    second = paired_device.call("session", "model", "set", "--session", session, "--provider", "fixture-provider", "--model", "fixture-model-2")
    assert second["noop"] is True
    assert second["selection"]["model"] == "fixture-model-2"
    # No new pending command and no second native selection.
    assert paired_device.call("operation", "list")["pending_commands"] == []
    assert scenario.dsh.process.control("model_counts")["selectSessionModel"] == before + 1


@pytest.mark.description("A model selected for one session never changes another session or the source default.")
def test_model_selection_is_isolated_between_sessions(scenario, paired_device):
    one, two = "qmq-model-one", "qmq-model-two"
    _session(scenario, one)
    _session(scenario, two)
    paired_device.sessions(scenario.host)
    source_default = _model(scenario, paired_device, two)["default_selection"]["model"]
    paired_device.call("session", "model", "set", "--session", one, "--provider", "fixture-provider", "--model", "fixture-model-2")
    other = _model(scenario, paired_device, two)
    assert other["next"] is None
    assert other["pending"] is None
    assert other["default_is_fallback"] is True
    assert other["default_selection"]["model"] == source_default
    selected = _model(scenario, paired_device, one)
    assert selected["next"]["model"] == "fixture-model-2"
    assert selected["default_is_fallback"] is False


@pytest.mark.description("After model set the next turn reports the new model as observed, not only a receipt echo.")
def test_model_next_turn_observes_the_selected_model(scenario, paired_device):
    session = "qmq-model-turn"
    _session(scenario, session)
    paired_device.sessions(scenario.host)
    paired_device.call("session", "model", "set", "--session", session, "--provider", "fixture-provider", "--model", "fixture-model-2")
    receipt = paired_device.send(session, "synthetic model turn", delivery="start-if-idle")["receipt"]
    assert receipt["status"] in ("received", "host_accepted", "succeeded")
    wait(
        lambda: (_model(scenario, paired_device, session)["last_used"] or {}).get("model")
        == "fixture-model-2"
    )
    observed = _model(scenario, paired_device, session)
    assert observed["last_used"]["model"] == "fixture-model-2"
    assert observed["pending"] is None
    assert observed["next"]["model"] == "fixture-model-2"


@pytest.mark.description("Model get for an unknown session is not_found/exit 3 and reads no source model.")
def test_model_get_unknown_session_is_not_found(scenario, paired_device):
    paired_device.sessions(scenario.host)
    before = scenario.dsh.process.control("model_counts")["readSessionModel"]
    code, values = paired_device.invoke("session", "model", "get", "--session", "qmq-unknown-session", success=False)
    assert code == 3
    assert values[-1]["error"]["code"] == "not_found"
    assert values[-1]["data"] == {"target": {"host_id": scenario.host, "session_id": "qmq-unknown-session"}}
    # The Host answered the read with not_found; the CLI did not invent an
    # empty selection or report an internal failure.
    assert "empty" not in values[-1]["error"]["message"].lower()
    assert scenario.dsh.process.control("model_counts")["readSessionModel"] >= before


@pytest.mark.description("An incomplete catalog explains a rejected provider and whether retrying is safe.")
def test_model_incomplete_catalog_set_failure_explains_retry(scenario, paired_device):
    session = "qmq-model-catalog"
    _session(scenario, session)
    paired_device.sessions(scenario.host)
    assert scenario.dsh.process.control("set_model_failures", session_id=session, text=["fixture-bad-provider"])
    catalog = paired_device.call("session", "models", "--host", scenario.host)
    assert catalog["complete"] is False
    assert catalog["failures"] == [{"id": "fixture-bad-provider", "name": "fixture-bad-provider", "code": "unavailable"}]
    code, values = paired_device.invoke(
        "session", "model", "set", "--session", session, "--provider", "fixture-bad-provider", "--model", "fixture-model",
        success=False,
    )
    message = values[-1]["error"]["message"]
    assert code == 2
    assert values[-1]["error"]["code"] == "invalid_argument"
    assert "no change was made" in message
    assert "retry only after" in message
    assert "whalerein session models" in message
    # The rejected selection did not change the session.
    assert _model(scenario, paired_device, session)["next"] is None
    # A successful selection explains that the source default is unconfirmed:
    # confirm with model get instead of resending the command.
    code, values = paired_device.invoke(
        "session", "model", "set", "--session", session, "--provider", "fixture-provider", "--model", "fixture-model",
    )
    assert code == 0
    assert values[-1]["data"]["model_selection"]["default_persistence"] == "unconfirmed"
