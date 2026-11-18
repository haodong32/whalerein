"""Session creation and fork pre-submission contracts and the child location."""

import concurrent.futures

import pytest


pytestmark = pytest.mark.chain


@pytest.mark.parametrize(
    "args",
    [
        pytest.param(
            ["session", "create", "--host", "{host}", "--cwd", ""],
            id="empty-cwd",
            marks=pytest.mark.description(
                "When session-create receives an empty working directory, the CLI rejects it with invalid input exit 2 and invokes no source creation."
            ),
        ),
        pytest.param(
            ["session", "create", "--host", "{host}", "--cwd", "relative/worktree"],
            id="relative-cwd",
            marks=pytest.mark.description(
                "When session-create receives a relative working directory, the CLI rejects it with invalid input exit 2 before submission."
            ),
        ),
        pytest.param(
            ["session", "create", "--host", "{host}", "--preset", "   "],
            id="blank-preset",
            marks=pytest.mark.description(
                "When session-create receives a blank agent preset, the CLI rejects it with invalid input exit 2 before submission."
            ),
        ),
        pytest.param(
            ["session", "create", "--host", "{host}", "--session-id", ""],
            id="empty-session-id",
            marks=pytest.mark.description(
                "When session-create receives an empty preferred session id, the CLI rejects it with invalid input exit 2 before submission."
            ),
        ),
    ],
)
def test_create_rejects_invalid_local_input(args, scenario, paired_device):
    """Invalid create values never reach the Host or create a journal entry."""
    before = scenario.dsh.process.control("gx_effect_counts")["executeHost"]
    code, values = paired_device.invoke(
        *[scenario.host if value == "{host}" else value for value in args], success=False
    )
    assert code == 2, f"expected invalid-input exit, got {code}: {values[-1:]}"
    assert values[-1]["error"]["code"] == "invalid_argument"
    assert scenario.dsh.process.control("gx_effect_counts")["executeHost"] == before
    assert paired_device.invoke('operation', 'list')[1][-1]["data"]["pending_commands"] == []


@pytest.mark.description(
    "When a completed turn is forked, the CLI reports the child working directory without a second silent lookup."
)
def test_fork_reports_the_child_working_directory(scenario, paired_device, session_id):
    """A confirmed child exposes the directory it shares with its parent."""
    paired_device.send(session_id, "synthetic fork parent")
    parent = scenario.dsh.snapshot(session_id)["summary"]
    result = paired_device.call('session', 'fork', session_id)
    receipt = result["lifecycle_receipt"]
    assert receipt["operation"] == "fork"
    assert receipt["parent_session_id"] == session_id
    assert receipt["disposition"] == "READY"
    assert receipt["session_id"] and receipt["session_id"] != session_id
    if parent.get("cwd"):
        assert result.get("working_directory") == parent["cwd"]
    child = scenario.dsh.snapshot(receipt["session_id"])["summary"]
    assert child["cwd"] == parent["cwd"]
    listed = next(
        row
        for row in paired_device.sessions(scenario.host)
        if row["session_id"] == receipt["session_id"]
    )
    assert listed["parent_session_id"] == session_id
    assert listed["origin"] == "fork"


@pytest.mark.description(
    "When a running source has no completed turn, session-fork reports a non-retryable FAILED lifecycle and creates no child."
)
def test_fork_without_a_completed_turn_is_a_proven_failure(scenario, paired_device, session_id):
    """FAILED is distinguishable from UNKNOWN so an automatic retry stays unsafe."""
    before = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    code, values = paired_device.invoke('session', 'fork', session_id, success=False)
    assert code != 0
    receipt = values[-1]["data"]["lifecycle_receipt"]
    assert receipt["operation"] == "fork"
    assert receipt["disposition"] == "FAILED"
    assert receipt["reason_code"]
    assert receipt.get("session_id") in (None, "")
    assert {row["session_id"] for row in paired_device.sessions(scenario.host)} == before


@pytest.mark.description(
    "When two independently paired devices fork the same completed parent concurrently, each gets a distinct child and no fork is replayed."
)
def test_two_devices_fork_the_same_parent_concurrently(scenario, paired_device, session_id):
    """Concurrent forks from separate processes create independent children."""
    paired_device.send(session_id, "synthetic concurrent fork parent")
    second = scenario.pair_second()
    second.sessions(scenario.host)
    parent_events = [
        event["type"] for event in scenario.dsh.snapshot(session_id)["events"]
    ]
    before = {row["session_id"] for row in paired_device.sessions(scenario.host)}

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        futures = [
            pool.submit(device.call, "session", "fork", session_id)
            for device in (paired_device, second)
        ]
        results = [future.result() for future in futures]

    children = {result["lifecycle_receipt"]["session_id"] for result in results}
    assert len(children) == 2, "concurrent forks shared one child"
    assert session_id not in children
    for result in results:
        receipt = result["lifecycle_receipt"]
        assert receipt["disposition"] == "READY"
        assert receipt["parent_session_id"] == session_id
        child = scenario.dsh.snapshot(receipt["session_id"])
        assert [event["type"] for event in child["events"]] == parent_events
    after = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    assert {row for row in after - before} == children
