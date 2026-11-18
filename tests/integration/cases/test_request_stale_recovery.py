"""Q02: a transient ``request_stale`` presence-publish failure degrades safely.

Access removes the polling claim before publishing ``request_stale``. If that
publish fails, the supported fallback is the HostRequest deadline. The case
faults the real MQTT publish through a loopback proxy, then requires a bounded
claim release, no fabricated or repeated answer, a later answer that settles,
and the failed stale release surfaced through Access' existing metrics API.

The open policy question is whether post-recovery delivery must restore the
short lease; the case accepts either documented safe outcome and never requires
a durable retry queue.
"""

import json
import os
import subprocess
import time
import urllib.request

import pytest

from environment import ROOT, wait
from presence_faults import PresencePublishFault

pytestmark = pytest.mark.chain

# HostRequest deadline is 30 seconds; allow container scheduling margin without
# turning the case into an unbounded wait.
DEADLINE_MARGIN = 15


def _answer_process(paired_device, session, batch, answer, command_id):
    return subprocess.Popen(
        [
            str(ROOT / "target/debug/whalerein"), "--json", "--yes",
            "--data-dir", str(paired_device.directory / "client"),
            "answer", "--session", session,
            "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
            "--version", str(batch["version"]), "--digest", batch["digest"],
            "--phase", batch["phase"], "--input", str(answer),
            "--command-id", command_id,
        ],
        cwd=ROOT,
        env={
            **os.environ,
            "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"),
            "WHALEREIN_LOG": "error",
        },
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _answer(paired_device, session, batch, answer, command_id):
    return paired_device.call(
        "answer", "--session", session,
        "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
        "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer),
        "--command-id", command_id,
    )


def _counter(name):
    """Read one counter from Access' existing Prometheus endpoint."""
    with urllib.request.urlopen("http://127.0.0.1:18080/metrics", timeout=5) as response:
        text = response.read().decode()
    for line in text.splitlines():
        if line.startswith(name + " "):
            return float(line.rsplit(" ", 1)[1])
        if line.startswith(name + "{"):
            return float(line.rsplit(" ", 1)[1])
    return 0.0


@pytest.mark.description(
    "A request_stale presence publish that fails transiently still releases the held answer by the bounded request deadline, surfaces the degraded release, and lets a later answer settle exactly once."
)
def test_failed_stale_presence_publish_releases_by_deadline_and_recovers(
    scenario, paired_device, session_id
):
    scenario.access.close()
    proxy = PresencePublishFault(scenario.host)
    original = scenario.config.read_text()
    updated = original.replace(
        '  url: "mqtt://127.0.0.1:1883"', f'  url: "{proxy.url}"', 1
    )
    assert updated != original
    scenario.config.write_text(updated)
    process = None
    try:
        scenario.start_access()
        wait(lambda: proxy.connections >= 1, timeout=20)

        scenario.dsh.process.control("request_question_open", session_id=session_id)
        scenario.dsh.process.control("hold_question_answer", session_id=session_id)
        batch = paired_device.call("questions", "--session", session_id)["batches"][0]
        assert batch["answerable"] and batch["phase"] == "QUESTION_OPEN"
        answer = scenario.directory / "stale-recovery-answer.json"
        answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
        before_calls = scenario.dsh.process.control("question_answer_calls")

        process = _answer_process(
            paired_device, session_id, batch, answer, "stale-fault-answer"
        )
        wait(
            lambda: scenario.dsh.process.control("question_answer_waiting", session_id=session_id),
            timeout=30,
        )
        before_stale = _counter("request_client_stale_total")
        connections = proxy.connections
        proxy.arm()
        started = time.monotonic()
        process.kill()
        process.communicate(timeout=15)

        # The armed publish really failed and was not delivered to the Host.
        wait(lambda: proxy.faults == 1, timeout=30)
        # The claim still has to release safely, at latest by the deadline.
        wait(
            lambda: not scenario.dsh.process.control("question_answer_waiting", session_id=session_id),
            timeout=30 + DEADLINE_MARGIN,
        )
        assert time.monotonic() - started < 30 + DEADLINE_MARGIN
        # The degraded stale release is surfaced through the existing metrics
        # API rather than hidden.
        assert _counter("request_client_stale_total") > before_stale
        # Access reconnected through the proxy after the transient failure.
        wait(lambda: proxy.connections > connections, timeout=20)

        # The first answer never settled: the batch is still open and answerable.
        remaining = paired_device.call("questions", "--session", session_id)["batches"]
        assert [item["batch_id"] for item in remaining] == [batch["batch_id"]]
        assert remaining[0]["answerable"] is True
        current = remaining[0]
        assert scenario.dsh.process.control("question_answer_calls") == before_calls + 1

        # Stop holding native answers so the recovered call can settle.
        assert scenario.dsh.process.control("release_question_answer", session_id=session_id)
        # A later answer settles once, and replaying its command id does not
        # invoke the source a second time.
        receipt = _answer(paired_device, session_id, current, answer, "stale-recovered-answer")
        assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
        assert paired_device.call("questions", "--session", session_id)["batches"] == []
        assert scenario.dsh.process.control("question_answer_calls") == before_calls + 2
        status, replay = scenario.replay_command(paired_device)
        assert status in (200, 202) and replay["commandId"] == receipt["command_id"]
        assert scenario.dsh.process.control("question_answer_calls") == before_calls + 2
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            try:
                process.communicate(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass
        scenario.access.close()
        scenario.config.write_text(original)
        scenario.start_access()
        proxy.close()
