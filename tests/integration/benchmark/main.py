"""Container benchmark entry point: one scripted streaming-reply scenario.

The response schedule is independent of the observer. Once the fixture accepts
the scripted send, fixed relative times drive the visible updates and the
commit; the observer only records when it reads each matching output record.
Every timestamp comes from one monotonic clock in this driver process.
"""

import hashlib
import json
import os
import platform
import select
import signal
import subprocess
import sys
import time
import traceback
import uuid
from collections import Counter
from pathlib import Path

sys.path.insert(0, "/workspace/tests/integration")

from environment import ROOT, Process, Scenario, wait

REPORTS = Path("/reports")
CONFIG = json.loads(os.environ.get("WHALEREIN_BENCHMARK_CONFIG", "{}"))
DEADLINE_MS = CONFIG.get("deadline_ms", 5000)
OBSERVATION_WINDOW_MS = CONFIG.get("observation_window_ms", 1000)
DELAY_MS = CONFIG.get("delay_ms", 0)
SEED = str(CONFIG.get("seed", 0))
ATTEMPTS = CONFIG.get("attempts", 30)
WARMUP = CONFIG.get("warmup_attempts", 3)
# Optional fault schedule (attempt index -> fault) used only to validate that
# each failure mode is accounted for; the default scenario has no faults.
FAULTS = {int(key): value for key, value in (CONFIG.get("faults") or {}).items()}

SETTLE_SECONDS = float(os.environ.get("WHALEREIN_BENCHMARK_SETTLE", "3"))

SCENARIO_ID = "streaming-reply"
SCENARIO_VERSION = 1

# Fixed workload: five visible updates starting at FIRST_UPDATE_MS, one per
# cadence step, then the final reply after one more step. A cadence at or below
# the product's own 20 ms coalescing window makes that window the thing being
# observed instead of hidden noise.
UPDATES = ["Stream", "ed ", "reply", " for", " benchmark"]
FINAL_TEXT = "".join(UPDATES)
GAP_MS = int(CONFIG.get("cadence_ms", 100))
FIRST_UPDATE_MS = 0
FINAL_AT_MS = FIRST_UPDATE_MS + GAP_MS * (len(UPDATES) - 1) + GAP_MS
DUPLICATE_AFTER_MS = 300
DUPLICATE_FINAL_MS = FINAL_AT_MS + DUPLICATE_AFTER_MS
SEND_TEXT = "benchmark streaming reply request"

SCENARIO = {
    "id": SCENARIO_ID,
    "version": SCENARIO_VERSION,
    "seed": SEED,
    "deadline_ms": DEADLINE_MS,
    "observation_window_ms": OBSERVATION_WINDOW_MS,
    "workload": {"updates": UPDATES, "gap_ms": GAP_MS, "delivery": "accepted"},
    "response": {
        "start_ms": FIRST_UPDATE_MS,
        "final_ms": FINAL_AT_MS,
        "delay_ms": DELAY_MS,
        "duplicate_final_ms": DUPLICATE_FINAL_MS,
        "final_text": FINAL_TEXT,
    },
}

GAP_SAMPLES = ("first_text_ms", "final_ms", "update_gaps_ms")
# A tail is reported only when its declared sample floor is met.
TAILS = (("median", 1), ("p95", 20), ("p99", 100))
UNAVAILABLE_TAIL = "insufficient_samples"


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
        self.configured = {
            "attempts": ATTEMPTS,
            "warmup_attempts": WARMUP,
            "concurrency": 1,
            "profile": "warm-client",
            "delay_ms": DELAY_MS,
            "deadline_ms": DEADLINE_MS,
            "observation_window_ms": OBSERVATION_WINDOW_MS,
            "seed": SEED,
        }
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
        self.send_exit_ms = None
        self.update_times = []
        self.finals = 0
        self.content = None
        self.send_code = None
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
            "send_exit_ms": self.send_exit_ms,
            "update_gaps_ms": self.gaps(),
            "update_count": len(self.update_times),
            "duplicate_finals": max(0, self.finals - 1),
        }
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
        execute(run, fixture)
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
        self.start_database()
        self.schema = "bench_" + uuid.uuid4().hex
        sql("CREATE SCHEMA " + self.schema)
        directory = REPORTS / "fixture"
        directory.mkdir(exist_ok=True)
        self.scenario = Scenario(directory, self.schema)
        self.run.session = self.scenario
        self.scenario.start_dsh()
        # The broker must be reachable before Access can report readiness here.
        self.start_broker()
        self.scenario.start_access()
        self.scenario.start_plugin()
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
        config = (ROOT / "deploy/compose/emqx/emqx.conf").read_text()
        config = config.replace("http://access:8081", "http://127.0.0.1:18081")
        config = config.replace(
            'data_dir = "/opt/emqx/data"', 'data_dir = "{}"'.format(REPORTS / "broker")
        )
        config += "\nauthorization.cache.enable = false\n"
        Path("/opt/emqx/etc/emqx.conf").write_text(config)
        Path("/opt/emqx/etc/api-keys.csv").write_text(
            (ROOT / "deploy/compose/emqx/api-keys.csv").read_text()
        )
        Path("/opt/emqx/etc/acl.conf").write_text(
            (ROOT / "deploy/compose/emqx/acl.conf").read_text()
        )
        self.broker = Process(["/opt/emqx/bin/emqx", "foreground"], REPORTS / "fixture")
        wait(
            lambda: (
                subprocess.run(
                    ["/opt/emqx/bin/emqx", "ctl", "status"],
                    capture_output=True,
                    timeout=5,
                    check=False,
                ).returncode
                == 0
            ),
            timeout=180,
            process=self.broker,
        )

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
    # Two paired devices, both through the real CLI: the observer holds the live
    # watch and the sender performs the measured send. A workload that reuses one
    # device for both would have the send displace the observer's live delivery.
    observer_device = fixture.scenario.pair()
    sender_device = pair_device(fixture.scenario, "sender")
    dsh = fixture.scenario.dsh.process
    for index in range(WARMUP + ATTEMPTS):
        phase = "warmup" if index < WARMUP else "measured"
        attempt = Attempt(len(run.attempts), phase)
        session = fixture.scenario.dsh.create_session()
        attempt.session = session
        observer_device.sync(session)
        measure(observer_device, sender_device, fixture, dsh, session, attempt)
        run.attempts.append(attempt.record(fixture.scenario.host))


def pair_device(scenario, name):
    """Pair an additional device with its own client directory and secret store."""
    from environment import Device, http

    directory = scenario.directory / name
    directory.mkdir(exist_ok=True)
    device = Device(directory)
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )
    device.identity = device.call("pair", invitation["pairingUri"])
    return device


def measure(observer_device, sender_device, fixture, dsh, session, attempt):
    observer = Observer(observer_device, fixture.scenario, session, attempt.stream)
    try:
        wait(observer.ready, timeout=120, process=observer.process)
        observer.arm()
        # Arm the schedule before any acceptance can happen: the observer is
        # already reading, so start-up cost never hides the first milestone.
        schedule(dsh, session, attempt)
        observer.dispatch(sender_device, session)
        observer.settle(attempt)
    finally:
        observer.close()
    settle(attempt)


def schedule(dsh, session, attempt):
    """Arm the fixture's scripted reply; the measured CLI send starts it.

    The plan carries only fixed relative times, so the fixture's schedule is
    independent of what the observer reads.
    """
    dsh.control(
        "bench_schedule",
        session_id=session,
        stream_id=attempt.stream,
        updates=list(UPDATES),
        gap_ms=GAP_MS,
        start_ms=FIRST_UPDATE_MS,
        final_ms=FINAL_AT_MS,
        delay_ms=DELAY_MS,
        duplicate_after_ms=DUPLICATE_AFTER_MS if attempt.fault == "duplicate" else None,
        fault=attempt.fault,
    )


def sleep_until(deadline):
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return
        time.sleep(min(remaining, 0.02))


class Observer:
    """Reads CLI stdout and records one monotonic time per matching record."""

    def __init__(self, device, scenario, session, stream):
        self.stream = stream
        self.buffer = bytearray()
        self.dispatch_at = None
        self.send = None
        self.final = None
        self.finals = 0
        self.updates = []
        self.content = None
        self.process = Process(
            [
                str(ROOT / "target/debug/whalerein"),
                "--json",
                "--data-dir",
                str(device.directory / "client"),
                "tail",
                "--session",
                session,
                "--timeout",
                str(int(DEADLINE_MS / 1000) + 60),
            ],
            scenario.directory,
            {
                "WHALEREIN_SECRET_BACKEND": "file:" + str(device.directory / "secrets"),
                "WHALEREIN_LOG": "info",
                "NO_COLOR": "1",
            },
            control=True,
        )

    def ready(self):
        self.process.output.seek(0)
        return "watch_ready" in self.process.output.read()

    def arm(self):
        self.read(0.05)
        self.buffer.clear()

    def dispatch(self, device, session):
        """Invoke the measured send on its own paired device."""
        self.dispatch_at = time.monotonic()
        self.send = subprocess.Popen(
            [
                str(ROOT / "target/debug/whalerein"),
                "--json",
                "--yes",
                "--data-dir",
                str(device.directory / "client"),
                "send",
                "--session",
                session,
                SEND_TEXT,
            ],
            cwd=ROOT,
            env={
                **os.environ,
                "WHALEREIN_SECRET_BACKEND": "file:" + str(device.directory / "secrets"),
                "WHALEREIN_LOG": "error",
                "NO_COLOR": "1",
            },
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

    def elapsed(self):
        return (time.monotonic() - self.dispatch_at) * 1000

    def read(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            ready, _, _ = select.select([self.process.child.stdout], [], [], 0.02)
            if not ready:
                continue
            chunk = os.read(self.process.child.stdout.fileno(), 65536)
            if not chunk:
                return
            self.buffer.extend(chunk)
            self.consume()

    def consume(self):
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
            event = value.get("data", {}).get("event")
            if isinstance(event, dict):
                self.observe(event)

    def observe(self, event):
        kind = event.get("type")
        if kind == "preview":
            if event.get("attempt_id") != self.stream:
                return
            text = event.get("text") or ""
            if not text.strip():
                return
            self.updates.append(round(self.elapsed(), 3))
        elif kind == "message":
            origin = event.get("origin") or {}
            if origin.get("attempt_id") != self.stream:
                return
            self.finals += 1
            if self.finals == 1:
                self.final = round(self.elapsed(), 3)
                self.content = text_of(event)

    def settle(self, attempt):
        """Stop at the deadline, then bound the observation window for duplicates."""
        while True:
            elapsed = self.elapsed()
            if self.finals and elapsed >= self.final + OBSERVATION_WINDOW_MS:
                break
            if elapsed >= DEADLINE_MS + OBSERVATION_WINDOW_MS:
                break
            self.read(0.02)
        if self.send.poll() is None:
            self.send.kill()
        try:
            self.send.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            self.send.kill()
            self.send.communicate(timeout=10)
        attempt.send_exit_ms = round(self.elapsed(), 3)
        attempt.send_code = self.send.returncode
        attempt.first_text_ms = self.updates[0] if self.updates else None
        attempt.update_times = list(self.updates)
        attempt.final_ms = self.final
        attempt.finals = self.finals
        attempt.content = self.content

    def close(self):
        if self.send is not None and self.send.poll() is None:
            self.send.kill()
        self.process.close()


def text_of(event):
    parts = []
    for block in event.get("blocks") or []:
        if block.get("type") == "text":
            parts.append(block.get("text") or "")
    return "".join(parts)


def settle(attempt):
    """One terminal outcome per attempt; observed partial milestones remain."""
    if attempt.send_code != 0:
        attempt.outcome = "product_error"
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
    if attempt.content != FINAL_TEXT:
        attempt.outcome = "incorrect_content"
        return
    if attempt.final_ms > DEADLINE_MS:
        # A reply past its deadline is a timeout, never a completion.
        attempt.outcome = "timeout_after_text"
        attempt.final_ms = None
        return
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
    for key in GAP_SAMPLES:
        values = []
        if key == "update_gaps_ms":
            values = sorted(
                float(gap) for item in complete_ids for gap in item["update_gaps_ms"]
            )
        else:
            values = sorted(
                float(item[key]) for item in complete_ids if item[key] is not None
            )
        samples[key] = entry(values)
    return {
        "schema_version": 1,
        "scenario": SCENARIO,
        "run": {
            "id": run.id,
            "started_at": run.started_at,
            "surface": "cli-stdout",
            "configuration": run.configured,
        },
        "environment": {
            "artifacts": artifacts(),
            "topology": {
                "database": "postgres-18-local",
                "broker": "emqx-5.8.8-local",
                "network": "none",
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
        "status": {
            "state": run.state,
            "reasons": list(run.reasons),
            "cleanup": run.cleanup
            if run.cleanup["result"] != "verified"
            else {"result": "verified", "reason": None},
        },
    }


def entry(values):
    """Nearest-rank quantiles for the tails the sample count can support."""
    document = {"count": len(values), "quantiles": {}, "unavailable": {}}
    if not values:
        document["reason"] = "no_samples"
        return document
    document["reason"] = None
    for name, minimum in TAILS:
        if len(values) < minimum:
            document["unavailable"][name] = UNAVAILABLE_TAIL
            continue
        fraction = {"median": 0.50, "p95": 0.95, "p99": 0.99}[name]
        document["quantiles"][name] = values[max(1, _ceil(fraction * len(values))) - 1]
    return document


def _ceil(value):
    return int(value) if value == int(value) else int(value) + 1


def artifacts():
    files = {
        "cli": ROOT / "target/debug/whalerein",
        "access": Path("/usr/local/bin/whalerein-access"),
        "plugin": ROOT / "host/dsh/package.json",
        "fixture": ROOT / "tests/integration/fixtures/dsh.mjs",
    }
    return {name: fingerprint(path) for name, path in files.items()}


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
