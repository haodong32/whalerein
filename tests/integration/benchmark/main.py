"""Container benchmark entry point: one scripted streaming-reply scenario.

Once Watch has attached, local fixture control starts the independent source
schedule. The source and observer use the same Linux monotonic clock. Reports
use the first actual source increment as time zero.
"""

import hashlib
import json
import os
import platform
import select
import signal
import socket
import subprocess
import tempfile
import sys
import time
import traceback
import uuid
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, "/workspace/tests/integration")

from benchmark import network, workflow
from client_environment import CutoverClient, isolated_keyring
from environment import ROOT, Process, Scenario, http, wait
from repo.benchmark import model

REPORTS = Path("/reports")
CONFIG = json.loads(os.environ.get("WHALEREIN_BENCHMARK_CONFIG", "{}"))
DEADLINE_MS = CONFIG.get("deadline_ms", 10000)
OBSERVATION_WINDOW_MS = CONFIG.get("observation_window_ms", 1000)
DELAY_MS = CONFIG.get("delay_ms", 0)
ATTEMPTS = CONFIG.get("attempts", 30)
WARMUP = CONFIG.get("warmup_attempts", 3)
NETWORK = CONFIG.get("network", model.network_settings("normal", "host"))
# Optional fault schedule (attempt index -> fault) used only to validate that
# each failure mode is accounted for; the default scenario has no faults.
FAULTS = {int(key): value for key, value in (CONFIG.get("faults") or {}).items()}

SETTLE_SECONDS = float(os.environ.get("WHALEREIN_BENCHMARK_SETTLE", "3"))

# The scheduled first increment bounds the observer's watchdog. Outcomes use
# the actual emission time, including when the fixture timer wakes slightly late.
SOURCE_START_ALLOWANCE_MS = 1000
RUN_CONFIG = {
    "attempts": ATTEMPTS,
    "warmup_attempts": WARMUP,
    "concurrency": 1,
    "profile": "warm-client",
    "delay_ms": DELAY_MS,
    "cadence_ms": CONFIG.get("cadence_ms", 100),
    "network": NETWORK,
    "workflow": CONFIG.get("workflow", "watch"),
    "deadline_ms": DEADLINE_MS,
    "observation_window_ms": OBSERVATION_WINDOW_MS,
    "seed": CONFIG.get("seed", 0),
}
SCENARIO = model.scenario(RUN_CONFIG)
FINAL_TEXT = SCENARIO["response"]["final_text"]
FINAL_BYTES = FINAL_TEXT.encode()
SOURCE_SIZES = model.source_sizes(SCENARIO)


class Run:
    """Owns every attempt fact and can publish them after any failure."""

    def __init__(self):
        self.id = "run-" + uuid.uuid4().hex[:12]
        self.started_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        self.attempts = []
        self.reasons = []
        self.state = "complete"
        self.cleanup = {"result": "verified", "reason": None}
        self.fixture = "unknown"
        self.network_probe = None
        self.network_counters = None
        self.session = None

    def fail(self, reason, state="invalid"):
        if reason not in self.reasons:
            self.reasons.append(reason)
        if state == "invalid" or self.state == "complete":
            self.state = state


class Attempt:
    """One attempt with its own scenario identity.

    Every attempt uses a distinct reply stream ID. The source projects previews
    by monotonically increasing revision per stream, so a reused ID would make
    later attempts' updates indistinguishable from replays and drop them.
    """

    def __init__(self, index, phase):
        self.identity = f"attempt-{index:04d}"
        self.stream = f"benchmark-attempt-{index:04d}"
        self.fault = FAULTS.get(index, "none")
        self.phase = phase
        self.outcome = None
        self.first_text_ms = None
        self.final_ms = None
        self.update_times = []
        self.source_times_ms = []
        self.outputs = []
        self.finals = 0
        self.preview_after_final = False
        self.watch_failed = False
        self.recovery = None
        self.history_reads = None
        self.content = None
        self.session = None

    def gaps(self):
        return [
            round(later - earlier, 3)
            for earlier, later in zip(self.update_times, self.update_times[1:])
        ]

    def record(self, host):
        document = {
            "id": self.identity,
            "phase": self.phase,
            "outcome": self.outcome or "product_error",
            "deadline_ms": DEADLINE_MS,
            "first_text_ms": self.first_text_ms,
            "final_ms": self.final_ms,
            "update_gaps_ms": self.gaps(),
            "update_count": len(self.update_times),
            "duplicate_finals": max(0, self.finals - 1),
            "preview_after_final": self.preview_after_final,
            "source_times_ms": self.source_times_ms,
            "outputs": self.outputs,
        }
        if self.recovery is not None:
            document["recovery"] = self.recovery
        if self.history_reads is not None:
            document["history_reads"] = self.history_reads
        if self.session:
            document["session_id"] = self.session
            document["host_id"] = host
        return document


def main():
    run = Run()

    def interrupt(*_):
        raise KeyboardInterrupt

    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)
    fixture = None
    try:
        run.fixture = fingerprint(ROOT / "tests/integration/fixtures/dsh.mjs")
        fixture = Fixture(run)
        fixture.start()
        before = network.counters()
        execute(run, fixture)
        run.network_counters = {
            key: value - before[key] for key, value in network.counters().items()
        }
    except KeyboardInterrupt:
        run.fail("run_interrupted", state="incomplete")
    except Exception:  # noqa: BLE001 - any setup failure still publishes what it knows
        run.fail("setup_failed")
        print("benchmark setup failed", file=sys.stderr)
        traceback.print_exc(limit=3, file=sys.stderr)
    finally:
        if fixture is not None and not fixture.close():
            run.cleanup = {"result": "unverified", "reason": "fixture_cleanup_failed"}
            run.fail("fixture_cleanup_failed")
    document = document_for(run)
    publish(document)
    measured = [item for item in run.attempts if item["phase"] == "measured"]
    if run.state != "complete" or any(
        item["outcome"] != "completed" for item in measured
    ):
        return 1
    if any(
        read["outcome"] != "completed"
        for attempt in measured for read in attempt.get("history_reads", [])
    ):
        return 1
    return 0


def publish(document):
    REPORTS.mkdir(exist_ok=True)
    path = REPORTS / "benchmark.json"
    try:
        path.write_text(
            json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        )
    except OSError:
        print("warning: could not write benchmark.json", file=sys.stderr)


class Fixture:
    """Owns FakeDSH, Access, one broker and the plugin for the whole run."""

    def __init__(self, run):
        self.run = run
        self.scenario = None
        self.broker = None
        self.database = None
        self.schema = None

    def start(self):
        network.configure(NETWORK)
        self.start_database()
        self.schema = "bench_" + uuid.uuid4().hex
        sql("CREATE SCHEMA " + self.schema)
        directory = REPORTS / "fixture"
        directory.mkdir(exist_ok=True)
        self.scenario = Scenario(directory, self.schema)
        # The shared Access/broker fabric stays on the reference address. Both
        # of the CLI's public endpoints use its separate address, including
        # HTTP recovery when MQTT delivery is disrupted.
        config = self.scenario.config.read_text()
        config = config.replace('listen: "127.0.0.1:18080"', 'listen: "0.0.0.0:18080"', 1)
        config = config.replace(
            'public_url: "http://127.0.0.1:18080"',
            f'public_url: "http://{network.CLIENT_ADDRESS}:18080"',
            1,
        )
        config = config.replace(
            'public_url: "mqtt://127.0.0.1:1883"',
            f'public_url: "mqtt://{network.CLIENT_ADDRESS}:{network.BROKER_PORT}"',
            1,
        )
        self.scenario.config.write_text(config)
        self.run.session = self.scenario
        self.scenario.start_dsh()
        before = time.monotonic_ns()
        source_clock = int(self.scenario.dsh.process.control("bench_clock_ns"))
        if not before <= source_clock <= time.monotonic_ns():
            raise RuntimeError("DSH and observer do not share a monotonic epoch")
        # The broker must be reachable before Access can report readiness here.
        self.start_broker()
        self.run.network_probe = network.probe()
        if not model.link_probe_matches(self.run.network_probe, NETWORK):
            raise RuntimeError("TCP RTT probes did not match the selected network link and profile")
        self.scenario.start_access(wildcard=True)
        self.scenario.start_plugin(
            extra_env={
                "WHALEREIN_MQTT_URL": f"mqtt://{network.HOST_BROKER_ADDRESS}:{network.BROKER_PORT}"
            }
        )
        # Give the hosted connection time to finish its first subscriptions
        # before the first attempt starts; this is setup, never measured.
        ready_at = time.monotonic() + SETTLE_SECONDS
        while time.monotonic() < ready_at:
            time.sleep(0.05)

    def start_database(self):
        """One disposable PostgreSQL 18 for this run, mirroring the test harness."""
        # initdb must chmod its data directory, which a host bind mount cannot
        # honor; the container tmpfs is discarded with it, like the test harness.
        directory = Path("/tmp/benchmark-postgres")
        directory.mkdir(parents=True, exist_ok=True)
        directory.chmod(0o755)
        data = directory / "data"
        data.mkdir(exist_ok=True)
        subprocess.run(
            ["chown", "postgres:postgres", str(data)], check=True, timeout=10
        )
        prefix = ["runuser", "-u", "postgres", "--"]
        result = subprocess.run(
            [
                *prefix,
                "/usr/lib/postgresql/18/bin/initdb",
                "-D",
                str(data),
                "-A",
                "trust",
            ],
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        if result.returncode != 0:
            print(
                "initdb failed: " + (result.stderr or result.stdout).strip()[-800:],
                file=sys.stderr,
            )
            raise RuntimeError("fixture database initialization failed")
        self.database = Process(
            [
                *prefix,
                "/usr/lib/postgresql/18/bin/postgres",
                "-D",
                str(data),
                "-p",
                "55432",
                "-h",
                "127.0.0.1",
            ],
            directory,
        )
        wait(
            lambda: (
                subprocess.run(
                    ["pg_isready", "-h", "127.0.0.1", "-p", "55432"],
                    capture_output=True,
                    check=False,
                    timeout=5,
                ).returncode
                == 0
            ),
            timeout=120,
            process=self.database,
        )
        subprocess.run(
            [
                "createdb",
                "-h",
                "127.0.0.1",
                "-p",
                "55432",
                "-U",
                "postgres",
                "whalerein_test",
            ],
            check=True,
            capture_output=True,
            timeout=30,
        )

    def start_broker(self):
        """Start the pinned Mosquitto 2.1.4 line with the WhaleRein plugin."""
        from environment import BROKER_PASSWORD, INTERNAL_TOKEN

        directory = REPORTS / "broker"
        directory.mkdir(parents=True, exist_ok=True)
        for parent in [directory, *list(directory.parents)[:3]]:
            try:
                parent.chmod(0o755)
            except OSError:
                pass
        password_file = directory / "access-password"
        token_file = directory / "internal-token"
        password_file.write_text(BROKER_PASSWORD)
        token_file.write_text(INTERNAL_TOKEN)
        config_path = directory / "mosquitto.conf"
        config_path.write_text(
            "\n".join(
                [
                    f"listener {network.BROKER_PORT} {network.BROKER_ADDRESS}",
                    "protocol mqtt",
                    f"listener {network.BROKER_PORT} {network.HOST_BROKER_ADDRESS}",
                    "protocol mqtt",
                    f"listener {network.BROKER_PORT} {network.CLIENT_ADDRESS}",
                    "protocol mqtt",
                    "set_tcp_nodelay true",
                    "allow_anonymous false",
                    "persistence false",
                    "max_packet_size 1048576",
                    "log_dest stdout",
                    "log_type error",
                    "connection_messages false",
                    "plugin /workspace/.verification/lib/whalerein.so",
                    "plugin_opt_access_username access",
                    f"plugin_opt_access_password_file {password_file}",
                    "plugin_opt_access_auth_url http://127.0.0.1:18081/internal/broker/connect",
                    f"plugin_opt_access_auth_token_file {token_file}",
                    "plugin_opt_auth_timeout_ms 500",
                    "plugin_opt_policy_ttl_ms 15000",
                    "",
                ]
            )
        )
        self.broker = Process(
            ["/usr/local/sbin/mosquitto", "-c", str(config_path)], REPORTS / "fixture"
        )

        def broker_ready():
            with socket.create_connection((network.BROKER_ADDRESS, network.BROKER_PORT), timeout=1):
                return True

        wait(broker_ready, timeout=120, process=self.broker)

    def close(self):
        ok = True
        try:
            if self.scenario is not None:
                self.scenario.close()
        except Exception:  # noqa: BLE001 - cleanup status is a fact, not a crash
            ok = False
        try:
            if self.broker is not None:
                self.broker.close()
            if self.schema is not None:
                sql("DROP SCHEMA IF EXISTS " + self.schema + " CASCADE")
        except Exception:  # noqa: BLE001
            ok = False
        if self.database is not None:
            # PostgreSQL stays healthy across child shutdown; stop it last.
            self.database.close()
        return ok


def ready():
    import urllib.request

    try:
        with urllib.request.urlopen("http://127.0.0.1:18080/readyz", timeout=2):
            return True
    except (OSError, ValueError):
        return False


def sql(statement):
    result = subprocess.run(
        [
            "psql",
            "-h",
            "127.0.0.1",
            "-p",
            "55432",
            "-U",
            "postgres",
            "-d",
            "whalerein_test",
            "-c",
            statement,
        ],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError("fixture database statement failed")


def execute(run, fixture):
    # Keep the Watch credentials outside the reports.
    with tempfile.TemporaryDirectory(prefix="whalerein-bench-keyring-") as directory:
        with isolated_keyring(Path(directory)) as keyring:
            scenario = fixture.scenario
            dsh = scenario.dsh.process
            history_spec = SCENARIO["workload"].get("history")
            history = None
            if history_spec:
                reader = pair_device(scenario, "history-reader", keyring)
                history = workflow.prepare_history(scenario, reader, history_spec)
            for index in range(WARMUP + ATTEMPTS):
                phase = "warmup" if index < WARMUP else "measured"
                attempt = Attempt(len(run.attempts), phase)
                # A fresh Device keeps previous CLI channels out of this attempt.
                device = pair_device(fixture.scenario, attempt.identity, keyring)
                session = fixture.scenario.dsh.create_session()
                attempt.session = session
                measure(device, fixture, dsh, session, attempt, history)
                run.attempts.append(attempt.record(fixture.scenario.host))
            if dsh.control("gx_effect_counts")["sendUserMessage"] != 0:
                raise RuntimeError("the Watch-only benchmark invoked a native send")


def pair_device(scenario, name, keyring):
    """Connect one fresh Standalone instance using a stdin invitation."""
    device = CutoverClient(scenario.directory, name, keyring)
    invite = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    device.call("host", "connect", "--invite-stdin", "--yes", input_text=invite["pairingUri"])
    return device


def measure(device, fixture, dsh, session, attempt, history):
    observer = Observer(device, fixture.scenario, session, attempt.stream)
    try:
        wait(observer.ready, timeout=120, process=observer.process)
        observer.arm()
        planned_start = start_source(dsh, session, attempt)
        specification = SCENARIO["workload"]
        if outage := specification.get("outage"):
            with ThreadPoolExecutor(max_workers=1) as executor:
                interrupted = executor.submit(
                    workflow.blackout, planned_start + outage["start_ms"] * 1_000_000,
                    outage["duration_ms"], NETWORK,
                )
                source_start = observer.settle(attempt, planned_start, dsh, session)
                attempt.recovery = workflow.recovery_observation(
                    attempt, *interrupted.result(), source_start, SOURCE_SIZES
                )
        elif history is not None:
            offsets = specification["history"]["start_ms"]
            with ThreadPoolExecutor(max_workers=len(offsets)) as executor:
                reads = [
                    executor.submit(history.read, planned_start + offset * 1_000_000, DEADLINE_MS)
                    for offset in offsets
                ]
                source_start = observer.settle(attempt, planned_start, dsh, session)
                attempt.history_reads = []
                for read in reads:
                    observation = read.result()
                    observation["at_ms"] = workflow.milliseconds(
                        observation.pop("started_ns") - source_start
                    )
                    attempt.history_reads.append(observation)
        else:
            observer.settle(attempt, planned_start, dsh, session)
    finally:
        observer.close()
    settle(attempt)


def start_source(dsh, session, attempt):
    """Start FakeDSH directly, with no CLI work besides the active Watch."""
    workload, response = SCENARIO["workload"], SCENARIO["response"]
    return int(dsh.control(
        "bench_start",
        session_id=session,
        stream_id=attempt.stream,
        updates=workload["updates"],
        gap_ms=workload["gap_ms"],
        start_ms=response["start_ms"],
        final_ms=response["final_ms"],
        delay_ms=response["delay_ms"],
        duplicate_after_ms=response["duplicate_final_ms"] - response["final_ms"]
        if attempt.fault == "duplicate" else None,
        fault=attempt.fault,
    ))


class Observer:
    """Reads CLI stdout and records one monotonic time per matching record."""

    def __init__(self, device, scenario, session, stream):
        self.stream = stream
        self.buffer = bytearray()
        self.armed = False
        self.preview = b""
        self.final = None
        self.finals = 0
        self.preview_after_final = False
        self.outputs = []
        self.content = None
        self.host = scenario.host
        self.session = session
        self.attached = False
        self.process = Process(
            device.args(
                "session", "watch", session, "--host", self.host, "--timeout",
                str(int(DEADLINE_MS / 1000) + 60),
            ),
            scenario.directory,
            {**device.env, "WHALEREIN_LOG": "error", "NO_COLOR": "1"},
            control=True,
        )

    def ready(self):
        self.read(0.05)
        return self.attached

    def arm(self):
        self.read(0.05)
        self.armed = True

    def read(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([self.process.child.stdout], [], [], 0.02)
            if not ready:
                continue
            chunk = os.read(self.process.child.stdout.fileno(), 65536)
            observed_at = time.monotonic_ns()
            if not chunk:
                return
            self.buffer.extend(chunk)
            self.consume(observed_at)

    def consume(self, observed_at):
        """Only complete newline-delimited envelopes are parseable."""
        lines = bytes(self.buffer).split(bytes([10]))
        self.buffer = bytearray(lines.pop())
        for line in lines:
            if not line.strip():
                continue
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if value.get("schema") != "whalerein/client/v1" or value.get("command") != "session.watch":
                continue
            event = value.get("data", {}).get("event")
            if not isinstance(event, dict) or event.get("session") != {
                "host_id": self.host, "session_id": self.session
            }:
                continue
            if event.get("type") == "state":
                self.attached = True
            elif self.armed:
                self.observe(event, observed_at)

    def observe(self, event, at_ns):
        kind = event.get("type")
        if kind == "preview":
            if event.get("attempt_id") != self.stream or event.get("kind") != "TEXT":
                return
            text = event.get("text") or ""
            if not text:
                return
            if self.finals:
                self.preview_after_final = True
                return
            visible = text.encode()
            self.preview = visible if event.get("replace") else self.preview + visible
            prefix = 0
            for actual, expected in zip(self.preview, FINAL_BYTES):
                if actual != expected:
                    break
                prefix += 1
            self.outputs.append({
                "at_ns": at_ns, "bytes": len(visible), "prefix_bytes": prefix, "final": False,
            })
        elif kind == "reset":
            self.preview = b""
        elif kind == "message":
            origin = event.get("origin") or {}
            if event.get("role") != "ASSISTANT" or (origin and origin.get("attempt_id") != self.stream):
                return
            self.finals += 1
            if self.finals == 1:
                self.final = at_ns
                self.content = workflow.text_of(event)
                prefix_bytes = (event.get("preview_prefix") or {}).get("text_bytes", 0)
                self.outputs.append({
                    "at_ns": at_ns, "bytes": len(self.content.encode()) - prefix_bytes,
                    "prefix_bytes": len(self.content.encode()), "final": True,
                })

    def settle(self, attempt, planned_start, dsh, session):
        """Collect output on the shared monotonic clock, then anchor it at source start."""
        watchdog = planned_start + (DEADLINE_MS + OBSERVATION_WINDOW_MS + SOURCE_START_ALLOWANCE_MS) * 1_000_000
        while time.monotonic_ns() < watchdog:
            if self.final is not None and time.monotonic_ns() >= self.final + OBSERVATION_WINDOW_MS * 1_000_000:
                break
            if self.process.child.poll() is not None:
                attempt.watch_failed = True
                break
            self.read(0.02)

        source = [int(value) for value in dsh.control("bench_text_times_ns", session_id=session)]
        start = source[0] if source else planned_start
        if source and abs(start - planned_start) > SOURCE_START_ALLOWANCE_MS * 1_000_000:
            raise RuntimeError("source start missed the fixture watchdog allowance")

        def relative(value):
            return round((value - start) / 1_000_000, 3)

        attempt.source_times_ms = [relative(value) for value in source]
        attempt.preview_after_final = self.preview_after_final
        attempt.outputs = [
            {"at_ms": relative(event["at_ns"]), "bytes": event["bytes"],
             "prefix_bytes": event["prefix_bytes"], "final": event["final"]}
            for event in self.outputs
        ]
        attempt.update_times = [event["at_ms"] for event in attempt.outputs if not event["final"]]
        attempt.first_text_ms = next(
            (event["at_ms"] for event in attempt.outputs if event["bytes"] > 0), None
        )
        if attempt.first_text_ms is not None and attempt.first_text_ms > DEADLINE_MS:
            attempt.first_text_ms = None
        attempt.final_ms = relative(self.final) if self.final is not None else None
        attempt.finals = self.finals
        attempt.content = self.content
        return start

    def close(self):
        self.process.close()


def settle(attempt):
    """One terminal outcome per attempt; observed partial milestones remain."""
    if attempt.watch_failed or attempt.preview_after_final:
        attempt.outcome = "product_error"
        attempt.final_ms = None
        return
    if attempt.finals > 1:
        attempt.outcome = "duplicate_final"
        attempt.final_ms = None
        return
    if attempt.final_ms is None:
        attempt.outcome = (
            "timeout_after_text"
            if attempt.first_text_ms is not None
            else "timeout_before_text"
        )
        return
    if attempt.final_ms > DEADLINE_MS:
        attempt.outcome = "late_reply"
        attempt.final_ms = None
        return
    if attempt.content != FINAL_TEXT:
        attempt.outcome = "incorrect_content"
        return
    if len(attempt.source_times_ms) != len(model.UPDATES) or not (
        0 <= attempt.source_times_ms[0] <= attempt.outputs[0]["at_ms"]
    ):
        raise RuntimeError("missing or invalid monotonic source timestamps")
    attempt.outcome = "completed"


def document_for(run):
    attempts = run.attempts
    measured = [item for item in attempts if item["phase"] == "measured"]
    complete_ids = [item for item in measured if item["outcome"] == "completed"]
    counts = Counter(item["outcome"] for item in measured)
    order = (
        "completed",
        "product_error",
        "timeout_before_text",
        "timeout_after_text",
        "late_reply",
        "incorrect_content",
        "duplicate_final",
    )
    outcome_counts = {name: counts.get(name, 0) for name in order}
    warmup = [item for item in attempts if item["phase"] == "warmup"]
    samples = {}
    for key in (*model.MILESTONES, "update_gaps_ms"):
        values = []
        if key == "update_gaps_ms":
            values = sorted(
                float(gap) for item in complete_ids for gap in item["update_gaps_ms"]
            )
        else:
            values = sorted(
                float(item[key]) for item in complete_ids if item[key] is not None
            )
        samples[key] = model.sample_entry(values)
    return {
        "schema_version": model.SCHEMA_VERSION,
        "scenario": SCENARIO,
        "run": {
            "id": run.id,
            "started_at": run.started_at,
            "surface": "cli-stdout",
            "configuration": RUN_CONFIG,
        },
        "environment": {
            "artifacts": artifacts(),
            "topology": {
                "database": "postgres-18-local",
                "broker": "mosquitto-2.1.4-local",
                "network": "none",
                "links": "host-mqtt-and-client-public-loopback-netem",
                "containers": 1,
            },
            "limits": {"cpus": os.cpu_count(), "memory_bytes": total_memory()},
            "versions": {
                "platform": platform.platform(),
                "python": platform.python_version(),
                "node": node_version(),
                "cli": executable_version(),
            },
            "fixture": {
                "name": "FakeDSH",
                "script": "streaming-reply",
                "transport": "unix",
                "connection": run.fixture,
            },
            "network_probe": run.network_probe,
            "network_counters": run.network_counters,
        },
        "attempts": attempts,
        "outcomes": {
            "counts": outcome_counts,
            "measured": len(measured),
            "correct_by_deadline": outcome_counts["completed"],
            "success_share": (outcome_counts["completed"] / len(measured))
            if measured
            else None,
            "warmup": {
                "attempts": len(warmup),
                "completed": sum(item["outcome"] == "completed" for item in warmup),
            },
        },
        "latency": {"samples": samples},
        "streaming": model.streaming_summary(
            attempts, len(FINAL_BYTES), SOURCE_SIZES, model.BURST_WINDOW_MS,
            display_delay=True,
        ),
        "diagnostics": model.diagnostic_summary(attempts, SOURCE_SIZES),
        "workflow_results": model.workflow_summary(attempts, RUN_CONFIG["workflow"]),
        "status": {
            "state": run.state,
            "reasons": list(run.reasons),
            "cleanup": run.cleanup
            if run.cleanup["result"] != "verified"
            else {"result": "verified", "reason": None},
        },
    }


def artifacts():
    files = {
        "cli": ROOT / "target/debug/whalerein",
        "access": Path("/workspace/.verification/bin/whalerein-access"),
        "broker_plugin": ROOT / ".verification/lib/whalerein.so",
        "fixture": ROOT / "tests/integration/fixtures/dsh.mjs",
    }
    values = {name: fingerprint(path) for name, path in files.items()}
    values["host"] = source_fingerprint(
        ("host/dsh", "host/runtime"),
        ("package.json", "pnpm-lock.yaml"),
    )
    values["harness"] = source_fingerprint(
        ("tests/integration/benchmark",),
        ("tests/integration/environment.py", "tests/integration/fixtures/plugin.mjs",
         "tests/integration/fixtures/adapter.mjs", "tests/integration/client_environment.py",
         "tools/repo/src/repo/benchmark/model.py"),
    )
    return values


def source_fingerprint(directories, files):
    paths = {ROOT / name for name in files}
    for folder in directories:
        paths.update(
            path for path in (ROOT / folder).rglob("*")
            if path.is_file() and path.suffix in (".js", ".mjs", ".json", ".py")
            and "node_modules" not in path.relative_to(ROOT).parts
        )
    digest = hashlib.sha256()
    for path in sorted(paths):
        digest.update(str(path.relative_to(ROOT)).encode() + b"\0")
        digest.update(fingerprint(path).encode() + b"\n")
    return "sha256:" + digest.hexdigest()


def fingerprint(path):
    try:
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return "unavailable"
    return "sha256:" + digest


def node_version():
    try:
        return subprocess.run(
            ["node", "--version"],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return "unavailable"


def executable_version():
    try:
        return subprocess.run(
            [str(ROOT / "target/debug/whalerein"), "--version"],
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return "unavailable"


def total_memory():
    try:
        return os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except (ValueError, OSError):
        return None


if __name__ == "__main__":
    sys.exit(main())
