"""Cache recovery preserves pairing and distinguishes storage from identity damage."""

import sqlite3

import pytest
from client_environment import remove_fixture_cache_key

pytestmark = pytest.mark.storage


@pytest.mark.description(
    "A file-level corrupt SQLite cache is quarantined and rebuilt on the next command while pairing identity stays valid."
)
def test_corrupt_cache_file_is_quarantined_and_pairing_preserved(
    scenario, paired_device, session_id
):
    paired_device.send(session_id, "cache corruption fixture")
    paired_device.observe_history(session_id)
    cache = paired_device.data_dir / "cache.sqlite3"
    cache.write_bytes(b"not a sqlite database")
    # The next cache access detects file corruption, quarantines the file and
    # rebuilds an empty cache. The remote session listing still works.
    sessions = paired_device.sessions(scenario.host)
    assert any(row["session_id"] == session_id for row in sessions)
    assert (paired_device.data_dir / "cache.sqlite3.quarantine").is_file()
    assert paired_device.hosts()[0]["online"] is True, "pairing must be preserved"
    assert paired_device.call("doctor")["commands"]["non_terminal"] == 0


@pytest.mark.description(
    "A missing cache key triggers a cache rebuild that preserves the pairing and never requires re-pairing."
)
def test_missing_cache_key_rebuilds_without_repairing_identity(
    scenario, paired_device, session_id
):
    paired_device.send(session_id, "cache key fixture")
    paired_device.observe_history(session_id)
    remove_fixture_cache_key(paired_device)

    sessions = paired_device.sessions(scenario.host)
    assert any(row["session_id"] == session_id for row in sessions)
    assert (paired_device.data_dir / "cache.sqlite3.quarantine").is_file()
    assert paired_device.hosts()[0]["host_id"] == scenario.host
    # A watch still attaches after the rebuild (it simply has no local history).
    code, _ = paired_device.invoke(
        "session", "watch", session_id, "--once", "--timeout", "2", success=False
    )
    assert code in (0, 7)


@pytest.mark.description(
    "A cache from a newer schema is refused with cache_error exit 1 without being destroyed, and repair restores access with pairing intact."
)
def test_a_future_cache_schema_is_refused_without_destroying_it(
    scenario, paired_device, session_id
):
    paired_device.send(session_id, "future schema fixture")
    paired_device.observe_history(session_id)
    cache = paired_device.data_dir / "cache.sqlite3"
    with sqlite3.connect(cache) as connection:
        connection.execute("UPDATE meta SET value='999' WHERE key='schema_version'")
    code, values = paired_device.invoke(
        "session", "list", "--host", scenario.host, success=False
    )
    assert code == 1, f"expected cache_error exit 1, got {values[-1:]}"
    assert values[-1]["error"]["code"] == "cache_error"
    assert not (paired_device.data_dir / "cache.sqlite3.quarantine").exists(), (
        "a future schema must not be destroyed by automatic recovery"
    )
    with sqlite3.connect(cache) as connection:
        connection.execute("UPDATE meta SET value='2' WHERE key='schema_version'")
    assert paired_device.sessions(scenario.host), "pairing and cache remain usable"
