"""ACC-01: an unavailable presence view must not terminally fail a send."""

import json
import select
import socket
import subprocess

import pytest

from environment import ROOT, wait
from journey_helpers import sql

pytestmark = pytest.mark.chain


def _listening(port=18080):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5):
            return True
    except OSError:
        return False


@pytest.mark.description(
    "A send submitted while Access presence is unavailable is queued and delivered after recovery instead of becoming host_offline."
)
def test_send_during_unavailable_presence_is_queued_and_delivered(
    scenario, paired_device
):
    """Access must distinguish 'presence not reconciled' from 'host offline'."""
    session = scenario.dsh.create_session()
    before = scenario.dsh.process.control("gx_effect_counts")["sendUserMessage"]
    original = scenario.config.read_text()
    # The same Device process keeps its established Noise channel. A fresh
    # CLI would need the offline Access query lane before it could submit.
    with subprocess.Popen(
        [str(ROOT / "target/debug/examples/delivery_driver"), str(paired_device.config_path),
         scenario.host, session, "--read-after-ready"],
        cwd=ROOT, env=paired_device.env(), stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    ) as sender:
        try:
            assert select.select([sender.stdout], [], [], 30)[0] and sender.stdout.readline().strip() == "ready"
            scenario.access.close()
            unreachable = original.replace(
                'url: "mqtt://127.0.0.1:1883"', 'url: "mqtt://127.0.0.1:19999"', 1
            )
            assert unreachable != original
            scenario.config.write_text(unreachable)
            scenario.start_access_process()
            wait(_listening, timeout=20, process=scenario.access)
            sender.stdin.write("presence-window message\n")
            sender.stdin.flush()
            commands = "FROM requests WHERE command_id <> ''"
            wait(lambda: sql(scenario.schema, "SELECT count(*) " + commands + " AND status='received'") == "1",
                 timeout=20)
            assert scenario.dsh.process.control("gx_effect_counts")["sendUserMessage"] == before
            scenario.access.close()
            scenario.config.write_text(original)
            scenario.start_access()
            wait(lambda: scenario.dsh.process.control("gx_effect_counts")["sendUserMessage"] == before + 1,
                 timeout=30)
            output, _ = sender.communicate(timeout=40)
            assert sender.returncode == 0 and json.loads(output)["status"] == "succeeded"
            assert sql(scenario.schema, "SELECT count(*) " + commands) == "1"
            assert sql(scenario.schema, "SELECT status " + commands) == "succeeded"
        finally:
            if sender.poll() is None:
                sender.kill()
            scenario.config.write_text(original)
