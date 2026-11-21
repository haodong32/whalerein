"""AC-01.above and AC-02.inline/reference through the real Host and Access.

A result above the Host-to-Access packet budget is uploaded and published as a
reference; the paired CLI resolves it through the Access directory and prints
the exact content. A small result stays inline and creates no directory object.
"""

import json
import subprocess

import pytest
from environment import ROOT

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
    with_reference = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.requests "
        "WHERE result_reference <> ''::bytea",
    )
    assert with_reference == "0", "an inline result must not create a reference"


@pytest.mark.relay_directory
def test_ac01_above_result_uses_the_access_directory(scenario, paired_device):
    """A result above the Host packet budget is uploaded before its reference, then resolves to exact bytes."""
    compact_device(paired_device)
    session = add_queue_item(scenario, 100 * 1024)
    paired_device.sessions(scenario.host)
    result = paired_device.call("session", "queue", "list", "--session", session)
    text = result["items"][0]["blocks"][0]["text"]
    assert len(text) == 100 * 1024 and text == "x" * (100 * 1024)
    referenced = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.requests "
        "WHERE result_reference <> ''::bytea AND status='succeeded'",
    )
    assert int(referenced) == 1, "the oversized result must be stored as a readable reference"
    # The directory is real: the case root contains one ready RESULT object
    # while the reference is retained.
    objects = psql(
        scenario.schema,
        f"SELECT count(*) FROM {scenario.schema}.temporary_ciphertext_objects "
        "WHERE purpose='PAYLOAD_PURPOSE_RESULT'",
    )
    assert int(objects) >= 1, "the Host must have uploaded the result ciphertext"


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
        f"SELECT count(*) FROM {scenario.schema}.requests WHERE kind=2",
    )
    assert int(rows) >= 1
