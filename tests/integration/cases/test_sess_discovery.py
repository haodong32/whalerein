"""Session discovery across authorized Hosts, offline cache fallback and scope.

These cases prove the cross-process behavior of `sessions --host`: a typo is a
not-found lookup, an offline Host uses its complete cached snapshot with the same
order, and two Host scopes cannot silently disagree.
"""

import pytest

from environment import wait

pytestmark = pytest.mark.chain


@pytest.mark.description(
    "When sessions names an unauthorized or mistyped Host, the CLI reports not-found exit 3 instead of a fingerprint conflict."
)
def test_sessions_unknown_host_is_not_found(scenario, paired_device, session_id):
    """An unpaired Host is a lookup failure, not a reason to pair again."""
    paired_device.sessions(scenario.host)
    code, values = paired_device.invoke(
        "sessions", "--host", "host-not-paired-" + scenario.schema[-8:], success=False
    )
    assert code == 3, f"expected not-found exit, got {code}: {values[-1:]}"
    error = values[-1]["error"]
    assert error["code"] == "not_found"
    assert "fingerprint" not in error["message"].lower()
    assert "pair" not in error["message"].lower()

    # The authorized Host still lists normally afterwards.
    assert paired_device.call("hosts")["hosts"], "authorized Host listing disappeared"


@pytest.mark.description(
    "When the Host goes offline, hosts reports online=false and sessions falls back to the complete cached snapshot with source=cache."
)
def test_hosts_offline_presence_and_cached_sessions(scenario, paired_device, session_id):
    """Offline discovery and the non-empty cache fallback are both explicit."""
    assert paired_device.call("hosts")["hosts"][0]["online"] is True
    online = paired_device.sessions(scenario.host)
    assert online, "the online listing must contain the fixture session"

    scenario.plugin.close()
    wait(
        lambda: not next(
            (row["online"] for row in paired_device.hosts() if row["host_id"] == scenario.host),
            True,
        ),
        timeout=30,
    )
    host = paired_device.call("hosts")["hosts"][0]
    assert host["host_id"] == scenario.host
    assert host["online"] is False

    code, values = paired_device.invoke("sessions", "--host", scenario.host)
    assert code == 0
    data = values[-1]["data"]
    assert data["source"] == "cache"
    assert data["complete"] is False
    assert [row["session_id"] for row in data["sessions"]] == [
        row["session_id"] for row in online
    ]


@pytest.mark.description(
    "When a device with no cached sessions queries an offline Host, sessions keeps the host-offline exit 5 instead of pretending the list is empty."
)
def test_sessions_offline_with_an_empty_cache_reports_offline(scenario, paired_device):
    """An empty cache cannot prove that the Host has no sessions."""
    second = scenario.pair_second()
    scenario.plugin.close()
    wait(
        lambda: not next(
            (row["online"] for row in paired_device.hosts() if row["host_id"] == scenario.host),
            True,
        ),
        timeout=30,
    )
    code, values = second.invoke("sessions", "--host", scenario.host, success=False)
    assert code == 5, f"expected host-offline exit, got {code}: {values[-1:]}"
    assert values[-1]["error"]["code"] in ("host_offline", "unavailable")


@pytest.mark.description(
    "When the Host is offline, cached session order matches the online most-recently-updated order instead of resorting by session id."
)
def test_sessions_offline_order_matches_the_online_order(scenario, paired_device):
    """Offline order must not jump when the same device reconnects."""
    for _ in range(3):
        scenario.dsh.create_session()
    online = [row["session_id"] for row in paired_device.sessions(scenario.host)]
    assert len(online) >= 3

    scenario.plugin.close()
    wait(
        lambda: not next(
            (row["online"] for row in paired_device.hosts() if row["host_id"] == scenario.host),
            True,
        ),
        timeout=30,
    )
    offline = [row["session_id"] for row in paired_device.sessions(scenario.host)]
    assert offline == online


@pytest.mark.description(
    "When global --host and sessions --host disagree, the CLI reports a conflict exit 8 and does not query either Host."
)
def test_sessions_host_scope_must_agree(scenario, paired_device, session_id):
    """Two explicit Host scopes must not silently select one of them."""
    before = paired_device.sessions(scenario.host)
    code, values = paired_device.invoke(
        "--host",
        scenario.host,
        "sessions",
        "--host",
        "host-other-" + scenario.schema[-6:],
        success=False,
    )
    assert code == 8, f"expected conflict exit, got {code}: {values[-1:]}"
    assert values[-1]["error"]["code"] == "conflict"
    assert "--host" in values[-1]["error"]["message"]

    matching = paired_device.call("--host", scenario.host, "sessions", "--host", scenario.host)
    assert [row["session_id"] for row in matching["sessions"]] == [
        row["session_id"] for row in before
    ]
