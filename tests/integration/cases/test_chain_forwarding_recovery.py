"""G12: a published command stays recoverable when Access loses Host confirmation.

The Access forwarding copy is an opaque status projection. Losing the Host's
acceptance or terminal result (and later seeing the Host go offline) may close
that copy as ``host_offline`` or exhaust uncertain publish retries as
``unknown``. Neither proves the command had no effect. The authenticated Host
journal remains the authority for the original terminal outcome, and Core
queries it without submitting the command again.
"""

import json
import threading

import pytest

from environment import wait
from forwarding_faults import AmbiguousHostRequestProxy, configure_requests
from journey_helpers import (
    effects,
    host_online,
    pending_commands,
    restart_access,
    sql,
    wait_online,
)

pytestmark = pytest.mark.chain

_REQUEST_COLUMNS = (
    "'request_id',request_id,'status',status,'forward_state',forward_state,"
    "'attempts',forward_attempts,'published_once',published_once,"
    "'error_code',coalesce(error_code,0)"
)


def _command_row(scenario, request_id=None):
    """Read the durable Access request copy, the only allowed status evidence."""
    clause = f"request_id='{request_id}'" if request_id else "kind=3"
    raw = sql(
        scenario.schema,
        "SELECT json_build_object("
        + _REQUEST_COLUMNS
        + f") FROM {scenario.schema}.requests WHERE {clause}"
        " ORDER BY created_at DESC LIMIT 1",
    )
    # psql also prints the leading SET command tag; only a JSON row counts.
    return json.loads(raw) if raw.startswith("{") else None


def _session(scenario, device):
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    return session


def _retry_due_while_offline(scenario, request_id):
    """Advance the published row's retry time, exercising the real forward loop.

    Access has already confirmed the Host is offline, so this only models the
    elapsed retry interval; it does not rewrite the request status or publish
    on the request's behalf.
    """
    row = _command_row(scenario, request_id)
    if row["status"] == "host_offline":
        return True
    sql(
        scenario.schema,
        f"UPDATE {scenario.schema}.requests SET forward_after = now() "
        f"WHERE request_id='{request_id}' AND status='received'",
    )
    return False


@pytest.mark.description(
    "A command published before Access loses its Host receipt is retried after the Host disconnects, and the original terminal outcome is recovered from the authenticated Host journal without a second source invocation."
)
def test_published_command_survives_offline_retry(scenario, paired_device):
    session = _session(scenario, paired_device)
    before = effects(scenario, "sendUserMessage")
    marker = scenario.directory / "plugin" / "gx-hold-command-results"
    marker.write_text("armed")
    outcome = {}

    def submit():
        outcome["send"] = paired_device.invoke(
            "send",
            "--session",
            session,
            "published before disconnect fixture",
            success=False,
        )

    sender = threading.Thread(target=submit, daemon=True)
    sender.start()
    try:
        wait(lambda: _command_row(scenario) is not None, timeout=30)
        row = _command_row(scenario)
        request_id = row["request_id"]
        # The request was really published and executed once, while both the
        # Host acceptance and the terminal result were held away from Access.
        wait(
            lambda: (current := _command_row(scenario, request_id))["published_once"]
            and current["forward_state"] == "published",
            timeout=30,
        )
        row = _command_row(scenario, request_id)
        assert row["status"] == "received"
        assert row["attempts"] == 1
        assert row["error_code"] == 0
        assert effects(scenario, "sendUserMessage") == before + 1

        # The Host disconnects before it can confirm the receipt. Access keeps
        # the published-but-still-received row until its retry becomes due.
        scenario.plugin.close()
        wait(lambda: host_online(paired_device, scenario.host) is False, timeout=40)
        wait(lambda: _retry_due_while_offline(scenario, request_id), timeout=30)
        closed = _command_row(scenario, request_id)
        assert closed["status"] == "host_offline"
        assert closed["forward_state"] == "none"
        assert closed["attempts"] >= 2
        assert closed["published_once"] is True
        assert effects(scenario, "sendUserMessage") == before + 1
    finally:
        # A failed assertion must not leave the submitting CLI running.
        sender.join(timeout=90)

    assert not sender.is_alive(), "the submitting CLI did not settle after the offline copy"
    code, values = outcome["send"]
    assert code == 7 and values[-1]["error"]["code"] == "unknown"
    pending = pending_commands(paired_device)
    assert [entry["request_id"] for entry in pending] == [request_id]
    command_id = pending[0]["command_id"]

    # Reconnect and deliver the authenticated original outcome through the real
    # Core command-recovery path; the source is asked only for its journal.
    marker.unlink(missing_ok=True)
    scenario.start_plugin()
    wait_online(paired_device, scenario.host)
    recovered = paired_device.call("command-recover", "--request", request_id)
    assert recovered["command_id"] == command_id
    assert recovered["status"] == "succeeded"
    assert recovered.get("message_id")
    assert effects(scenario, "sendUserMessage") == before + 1
    assert pending_commands(paired_device) == []
    # The misleading final Access copy is preserved, not rewritten: recovery
    # established the business result through the authenticated Host journal.
    assert _command_row(scenario, request_id)["status"] == "host_offline"


@pytest.mark.description(
    "A command whose publish return is ambiguous exhausts retries as unknown while its source effect happened once, and the authenticated Host journal still returns the original terminal outcome."
)
def test_ambiguous_publish_recovers_authenticated_outcome(scenario, paired_device):
    proxy = AmbiguousHostRequestProxy(scenario.host)
    try:
        configure_requests(
            scenario,
            broker_url=proxy.url,
            forward_interval="100ms",
            retry_interval="200ms",
            max_attempts=3,
        )
        restart_access(scenario)
        wait_online(paired_device, scenario.host)
        session = _session(scenario, paired_device)
        before = effects(scenario, "sendUserMessage")
        marker = scenario.directory / "plugin" / "gx-hold-command-results"
        marker.write_text("armed")
        code, values = paired_device.invoke(
            "send",
            "--session",
            session,
            "ambiguous publish fixture",
            success=False,
        )
        assert code == 7 and values[-1]["error"]["code"] == "unknown"
        row = _command_row(scenario)
        request_id = row["request_id"]
        # The proxy forwarded at least one command publish to the broker before
        # losing its acknowledgement, so the source effect really happened.
        # Later retries may fail before reaching the proxy depending on the
        # reconnect timing; the exhausted copy must stay unknown either way.
        assert effects(scenario, "sendUserMessage") == before + 1
        assert proxy.faults >= 1
        # Exhausted retries leave an uncertain copy. The command is never
        # claimed to have had no effect.
        assert row["status"] == "unknown"
        assert row["error_code"] == 18  # whalerein.v1.ErrorCode.unavailable
        assert row["published_once"] is False
        assert row["attempts"] == 3
        pending = pending_commands(paired_device)
        assert [entry["request_id"] for entry in pending] == [request_id]
        command_id = pending[0]["command_id"]

        proxy.disarm()
        marker.unlink(missing_ok=True)
        recovered = paired_device.call("command-recover", "--request", request_id)
        assert recovered["command_id"] == command_id
        assert recovered["status"] == "succeeded"
        assert recovered.get("message_id")
        assert effects(scenario, "sendUserMessage") == before + 1
        assert pending_commands(paired_device) == []
        assert _command_row(scenario, request_id)["status"] == "unknown"
    finally:
        proxy.close()


@pytest.mark.description(
    "A never-forwarded request to a Host known to be offline reports host_offline without invoking the source."
)
def test_never_forwarded_request_to_offline_host_has_no_source_effect(scenario, paired_device):
    session = _session(scenario, paired_device)
    scenario.plugin.close()
    wait(lambda: host_online(paired_device, scenario.host) is False, timeout=40)
    before = effects(scenario, "sendUserMessage")
    code, values = paired_device.invoke(
        "send",
        "--session",
        session,
        "known offline fixture",
        success=False,
    )
    assert code != 0
    assert values[-1]["error"]["code"] == "host_offline"
    assert effects(scenario, "sendUserMessage") == before
    # Core refused before submitting a command: no command identity exists,
    # so nothing can have been forwarded or executed for it.
    assert _command_row(scenario) is None
