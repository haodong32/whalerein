"""Container benchmark entry point: raw case facts for one scripted case.

The fixture emits raw observed facts only. ``repo.benchmark.model`` derives
attempt outcomes, durations, deadline snapshots, terminal integrity,
percentiles, exemplar selection and every verdict. Nothing in this file
classifies an attempt or computes a latency.

Facts are published at every exit path: the seeded slot plan before any
producer starts, the partial plan/facts when startup fails, and the final
document after the outlets are drained. Cleanup status is kept separate from
the business facts.
"""

import hashlib
import json
import os
import platform
import select
import signal
import socket
import subprocess
import sys
import tempfile
import time
import traceback
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, "/workspace/tests/integration")

from client_environment import CutoverClient, isolated_keyring
from environment import ROOT, Process, Scenario, http, wait
from repo.benchmark import model

from benchmark import capture as capture_module
from benchmark import clock as clock_module
from benchmark import events as events_module
from benchmark import facts, intervals, network, slots, workflow
from benchmark import outlet as outlet_module
from benchmark import sampler as sampler_module
from benchmark import source as source_module

REPORTS = Path(os.environ.get("WHALEREIN_BENCH_REPORTS", "/reports"))
CONFIG = json.loads(os.environ.get("WHALEREIN_BENCHMARK_CONFIG", "{}"))
MODE = (
    CONFIG.get("observation_mode")
    or os.environ.get("WHALEREIN_BENCHMARK_OBSERVATION")
    or "off"
)
DEADLINE_MS = CONFIG.get("deadline_ms", 10000)
OBSERVATION_WINDOW_MS = CONFIG.get("observation_window_ms", 1000)
DELAY_MS = CONFIG.get("delay_ms", 0)
ATTEMPTS = CONFIG.get("attempts", model.DEFAULT_ATTEMPTS)
WARMUP = CONFIG.get("warmup_attempts", model.DEFAULT_WARMUPS)
SEED = CONFIG.get("seed", 0)
NETWORK = CONFIG.get("network", model.network_settings("normal", "host"))
SAMPLER_INTERVAL_MS = max(100, int(CONFIG.get("sampler_interval_ms", 1000)))
# Trace sidecar budget. Space is reserved for loss/coverage finalization before
# the cap is applied; once it is reached detail stops being retained.
TRACE_CAP_BYTES = int(CONFIG.get("trace_cap_bytes", 8 << 20))
TRACE_RESERVE_BYTES = int(CONFIG.get("trace_reserve_bytes", 64 << 10))

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
    "seed": SEED,
    "observation_mode": MODE,
    "cpu_profile": bool(CONFIG.get("cpu_profile", False)),
    "p95_scope": CONFIG.get("p95_scope", "pooled"),
}
SCENARIO = facts.scenario_controls(model.scenario(RUN_CONFIG))
FINAL_TEXT = SCENARIO["response"]["final_text"]
FINAL_BYTES = FINAL_TEXT.encode()
SOURCE_SIZES = model.source_sizes(SCENARIO)
CONTROLS = SCENARIO["workload"].get("controls") or {}

SETTLE_SECONDS = float(os.environ.get("WHALEREIN_BENCHMARK_SETTLE", "3"))
SOURCE_START_ALLOWANCE_MS = 1000


class Run:
    """Owns every raw fact and can publish what it knows after any failure."""

    def __init__(self):
        self.capture = capture_module.Capture(MODE, reports=REPORTS)
        self.capture_origin_ns = clock_module.monotonic_ns()
        self.started_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        self.schedule = slots.plan(RUN_CONFIG)
        self.attempts = []
        self.reasons = []
        self.state = "complete"
        self.cleanup = {"result": "verified", "reason": None}
        self.fixture = "unknown"
        self.network_probe = None
        self.network_counters = None
        self.session = None
        self.clock = clock_module.ClockAdmission.local()
        self.pending_clocks = []
        self.sampler = None
        self.trace = None
        self.manifest = None
        self.outlet = None
        self.fixture_lifetime = "fx-" + uuid.uuid4().hex[:16]
        self.exporter_cpu_ms = None
        if MODE == "trace":
            self.manifest = events_module.load_manifest(
                ROOT / "contracts/latency/observation.json"
            )
            self.outlet = outlet_module.Outlet(
                path=str(REPORTS / "outlet" / f"fixture-{self.fixture_lifetime}.ndjson"),
                run_id=self.capture.run_id,
                producer="fixture",
                lifetime=self.fixture_lifetime,
                manifest=self.manifest,
            )
            self.outlet.__enter__()

    @property
    def id(self):
        return self.capture.run_id

    def emit(self, event, ts=None, **data):
        if self.outlet is not None:
            self.outlet.emit(event, ts=ts, **data)

    def fail(self, reason, state="invalid"):
        if reason not in self.reasons:
            self.reasons.append(reason)
        if state == "invalid" or self.state == "complete":
            self.state = state

    def outlet_flush(self):
        if self.outlet is not None:
            self.outlet.flush()

    def close(self):
        if self.outlet is not None:
            self.outlet.close()
            self.outlet = None


def main():
    run = Run()

    def interrupt(*_):
        raise KeyboardInterrupt

    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGTERM, interrupt)
    fixture = None
    try:
        # The seeded plan is persisted before any producer starts.
        publish_plan(run)
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
        if run.sampler is not None:
            run.sampler.stop()
        if fixture is not None and not fixture.close():
            run.cleanup = {"result": "unverified", "reason": "fixture_cleanup_failed"}
            run.fail("fixture_cleanup_failed")
    try:
        capture_trace(run)
    except Exception:  # noqa: BLE001 - export failure must not drop known facts
        run.fail("trace_export_failed", state="incomplete")
        _private_staging(REPORTS / "outlet")
        traceback.print_exc(limit=3, file=sys.stderr)
    try:
        document = document_for(run)
    except Exception:  # noqa: BLE001 - fall back to the facts known at exit
        run.fail("document_build_failed")
        traceback.print_exc(limit=3, file=sys.stderr)
        document = fallback_document(run)
    run.close()
    try:
        facts.publish(REPORTS / "benchmark.json", document)
        publish_checkpoint("benchmark.json")
    except OSError:
        run.fail("publish_failed")
        print("warning: could not write benchmark.json", file=sys.stderr)
    return exit_code(run)


def publish_plan(run):
    """Persist the schedule and an initial facts checkpoint before startup."""
    try:
        REPORTS.mkdir(exist_ok=True)
        facts.publish(REPORTS / "schedule.json", run.schedule)
        document = facts.document(
            run=facts.run_block(
                capture=run.capture,
                configuration=RUN_CONFIG,
                started_at=run.started_at,
                fingerprint=fingerprint_block(run),
            ),
            scenario=SCENARIO,
            environment=environment_for(run),
            schedule=run.schedule,
            attempts=[],
            trace=facts.trace_block(mode=MODE),
            status=facts.status_block(state="incomplete", reasons=["planned_not_started"]),
        )
        facts.publish(REPORTS / "benchmark.json", document)
    except OSError:
        run.fail("plan_publish_failed")
        raise


def publish_checkpoint(name):
    _private(REPORTS / name)


class Fixture:
    """Owns FakeDSH, Access, one broker and the plugin for the whole run."""

    def __init__(self, run):
        self.run = run
        self.scenario = None
        self.broker = None
        self.database = None
        self.schema = None
        self.lifetimes = {
            "dsh": "dsh-" + uuid.uuid4().hex[:16],
            "host": "host-" + uuid.uuid4().hex[:16],
            "access": "acc-" + uuid.uuid4().hex[:16],
        }

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
        self.scenario.configure_relay_directory(
            max_objects=1024, max_active_uploads=32, max_host_bytes=512 << 20,
            max_node_bytes=1 << 30,
        )
        self.run.session = self.scenario
        self.register(
            "fixture",
            os.getpid(),
            reading=lambda: clock_module.monotonic_ns(),
            outlet=None,
        )
        self.scenario.env.update(
            self.run.capture.environment("access", self.lifetimes["access"])
        )
        self.scenario.start_dsh(
            extra_env=self.run.capture.environment("dsh", self.lifetimes["dsh"])
        )
        self.register(
            "dsh",
            self.scenario.dsh.process.child.pid,
            reading=lambda: int(self.scenario.dsh.process.control("bench_clock_ns")),
            outlet=None,
        )
        self.start_broker()
        self.run.network_probe = network.probe()
        if not model.link_probe_matches(self.run.network_probe, NETWORK):
            raise RuntimeError("TCP RTT probes did not match the selected network link and profile")
        self.scenario.start_access(wildcard=True)
        self.register(
            "access",
            self.scenario.access.child.pid,
            outlet=self.run.capture.outlet_path("access", self.lifetimes["access"]),
        )
        self.scenario.start_plugin(
            extra_env={
                "WHALEREIN_MQTT_URL": f"mqtt://{network.HOST_BROKER_ADDRESS}:{network.BROKER_PORT}",
                **self.run.capture.environment("host", self.lifetimes["host"]),
            }
        )
        self.register(
            "host",
            self.scenario.plugin.child.pid,
            outlet=self.run.capture.outlet_path("host", self.lifetimes["host"]),
        )
        self.start_sampler()
        # Give the hosted connection time to finish its first subscriptions
        # before the first attempt starts; this is setup, never measured.
        ready_at = time.monotonic() + SETTLE_SECONDS
        while time.monotonic() < ready_at:
            time.sleep(0.05)

    def register(self, producer, pid, reading=None, outlet=None):
        before_ns = clock_module.monotonic_ns()
        value = reading() if callable(reading) else reading
        if value is None:
            value = clock_module.monotonic_ns()
        entry = self.run.clock.register(
            producer, pid=pid, reading=int(value), before_ns=before_ns
        )
        if outlet is not None and entry.get("verified") and entry.get("reason") == "membership_only":
            self.run.pending_clocks.append(
                {"entry": entry, "outlet": outlet, "before_ns": before_ns, "after_ns": None}
            )
        self.run.emit(
            "fixture_clock_check",
            producer=producer,
            reading_ns=int(value),
            before_ns=int(before_ns),
            after_ns=int(clock_module.monotonic_ns()),
            clock_source=clock_module.CLOCK_SOURCE,
            ok=bool(entry.get("verified")),
            reason=entry.get("reason") or "",
        )
        return entry

    def start_sampler(self):
        self.run.sampler = sampler_module.EnvironmentSampler(
            interval_ms=SAMPLER_INTERVAL_MS,
            origin_ns=self.run.capture_origin_ns,
            socket_probe=sampler_module.default_ss_probe,
            node_probe=lambda: self.scenario.dsh.process.control("bench_runtime"),
            access_probe=sampler_module.default_access_probe,
            qdisc_probe=network.counters,
            roles={"host": "host", "client": "client"},
            addresses={
                "host": network.HOST_BROKER_ADDRESS,
                "client": network.CLIENT_ADDRESS,
            },
        ).start()

    def close(self):
        ok = True
        if self.run.sampler is not None:
            self.run.sampler.stop()
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
            self.database.close()
        return ok

    # -- existing fixture process plumbing ---------------------------------

    def start_database(self):
        directory = Path("/tmp/benchmark-postgres")
        directory.mkdir(parents=True, exist_ok=True)
        directory.chmod(0o755)
        data = directory / "data"
        data.mkdir(exist_ok=True)
        subprocess.run(["chown", "postgres:postgres", str(data)], check=True, timeout=10)
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
                "createdb", "-h", "127.0.0.1", "-p", "55432", "-U", "postgres",
                "whalerein_test",
            ],
            check=True,
            capture_output=True,
            timeout=30,
        )

    def start_broker(self):
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


def sql(statement):
    result = subprocess.run(
        [
            "psql", "-h", "127.0.0.1", "-p", "55432", "-U", "postgres",
            "-d", "whalerein_test", "-c", statement,
        ],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError("fixture database statement failed")


def execute(run, fixture):
    keyring_directory = tempfile.TemporaryDirectory(prefix="whalerein-bench-keyring-")
    try:
        with isolated_keyring(Path(keyring_directory.name)) as keyring:
            scenario = fixture.scenario
            history = attachment = None
            if CONTROLS.get("history"):
                reader = pair_device(scenario, "history-reader", keyring)
                history = workflow.prepare_history(scenario, reader, CONTROLS["history"])
            if CONTROLS.get("attachment"):
                attachment_reader = pair_device(scenario, "attachment-reader", keyring)
                attachment = workflow.prepare_attachment(
                    scenario, attachment_reader, CONTROLS["attachment"]
                )
            for slot in slots.ordered_slots(run.schedule):
                try:
                    record_slot(run, fixture, slot, keyring, history, attachment)
                except KeyboardInterrupt:
                    run.fail("run_interrupted", state="incomplete")
                    raise
    finally:
        keyring_directory.cleanup()


def pair_device(scenario, name, keyring):
    device = CutoverClient(scenario.directory, name, keyring)
    invite = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    device.call("host", "connect", "--invite-stdin", "--yes", input_text=invite["pairingUri"])
    return device


def record_slot(run, fixture, slot, keyring, history, attachment):
    """Run one planned slot and append exactly one raw attempt fact."""
    index = slot["slot"]
    phase, arm, pair_id = slot["phase"], slot["arm"], slot["pair"]
    stream = f"benchmark-slot-{index:04d}"
    attempted_id = attempt_slot_id(run, stream)
    run.emit(
        "fixture_slot", slot=index, phase=phase, arm=arm, pair=pair_id, state="started"
    )
    started_ns = clock_module.monotonic_ns()
    attempted = {
        "status": "started",
        "failure": {"attribution": None},
        "setup_errors": [],
        "source": {"times_ms": [], "sizes": []},
        "outputs": [],
        "observation": None,
        "recovery": None,
        "reads": None,
        "origin_ns": started_ns,
        "origin": source_module.FIRST_EMISSION,
        "entered": False,
        "id": attempted_id,
        "stream": stream,
    }
    observer = None
    device = None
    try:
        device = pair_device(fixture.scenario, f"{phase}-{index:04d}", keyring)
        session = fixture.scenario.dsh.create_session()
        observer = Observer(run, device, fixture.scenario, session, stream, index)
        wait(observer.ready, timeout=120, process=observer.process)
        observer.arm()
        run.emit(
            "fixture_slot", slot=index, phase=phase, arm=arm, pair=pair_id,
            state="measurement_entered",
        )
        attempted["entered"] = True
        planned_start = start_source(fixture.scenario.dsh.process, session, stream)
        attempted["planned_start_ns"] = planned_start
        if CONTROLS.get("outage"):
            measured = measure_recovery(run, fixture, observer, planned_start, stream)
        elif history is not None and arm == "bulk":
            measured = measure_reads(
                run, observer, fixture.scenario, planned_start, session, stream,
                history, attachment,
            )
        else:
            measured = measure_watch(run, observer, planned_start, stream)
        apply_measurement(run, attempted, measured)
        attempted["status"] = "finished"
        run.emit(
            "fixture_slot", slot=index, phase=phase, arm=arm, pair=pair_id,
            state="finished",
        )
    except KeyboardInterrupt:
        attempted["status"] = "interrupted"
        attempted["failure"] = {"attribution": "fixture", "reason": "run_interrupted"}
        run.emit(
            "fixture_slot", slot=index, phase=phase, arm=arm, pair=pair_id,
            state="interrupted",
        )
        append_attempt(run, slot, attempted)
        raise
    except Exception as error:  # noqa: BLE001 - a failed slot is an accounted fact
        status = "interrupted" if attempted["entered"] else "setup_failed"
        attempted["status"] = status
        attempted["failure"] = {
            "attribution": "fixture",
            "reason": stable_reason(error),
        }
        attempted["setup_errors"] = [] if attempted["entered"] else [stable_reason(error)]
        run.emit(
            "fixture_slot", slot=index, phase=phase, arm=arm, pair=pair_id, state=status
        )
        print(f"slot {index} {status}: {stable_reason(error)}", file=sys.stderr)
    finally:
        if observer is not None:
            observer.close()
        if device is not None:
            close_device(device)
    append_attempt(run, slot, attempted)


def append_attempt(run, slot, attempted):
    observation = attempted["observation"]
    if observation is None:
        observation = facts.observation_block(
            started_ns=attempted.get("origin_ns", clock_module.monotonic_ns()),
            ended_ns=clock_module.monotonic_ns(),
            cutoff_ns=attempted.get("origin_ns", clock_module.monotonic_ns()),
            origin_ns=attempted.get("origin_ns", clock_module.monotonic_ns()),
            origin=attempted.get("origin", source_module.FIRST_EMISSION),
            early_stop="interrupted" if attempted["status"] == "interrupted" else "error",
        )
    fact = facts.attempt(
        slot=slot["slot"],
        phase=slot["phase"],
        arm=slot["arm"],
        pair_id=slot["pair"],
        status=attempted["status"],
        source=attempted["source"],
        outputs=attempted["outputs"],
        observation=observation,
        failure=attempted["failure"],
        setup_errors=attempted["setup_errors"],
        recovery=attempted["recovery"],
        reads=attempted["reads"],
        identity=attempted.get("id"),
    )
    run.attempts.append(fact)


def measure_watch(run, observer, planned_start, stream):
    ended_ns, early_stop = observer.settle(planned_start)
    emissions = observer.emissions()
    origin_ns, origin_name = source_module.origin(
        "watch", first_emission_ns=emissions[0] if emissions else planned_start
    )
    return {
        "emissions_ns": emissions,
        "origin_ns": origin_ns,
        "origin": origin_name,
        "ended_ns": ended_ns,
        "early_stop": early_stop,
        "outputs": observer.outputs_ns(),
        "recovery": None,
        "reads": None,
        "stream": stream,
    }


def measure_reads(run, observer, scenario, planned_start, session, stream, history, attachment):
    history_offsets = (CONTROLS.get("history") or {}).get("start_ms", [])
    attachment_offsets = (CONTROLS.get("attachment") or {}).get("start_ms", [])
    readers = len(history_offsets) + len(attachment_offsets)
    reads = []
    with ThreadPoolExecutor(max_workers=max(1, readers)) as executor:
        def spawn(pid):
            run.clock.register("cli", pid=pid)

        def read_environment(op):
            return run.capture.environment(
                "cli", f"{op}-{uuid.uuid4().hex[:16]}"
            )

        history_futures = [
            executor.submit(
                history.read, planned_start + offset * 1_000_000, DEADLINE_MS,
                environment=read_environment("history"), on_spawn=spawn,
            )
            for offset in history_offsets
        ]
        attachment_futures = [
            executor.submit(
                attachment.read, planned_start + offset * 1_000_000, DEADLINE_MS,
                environment=read_environment("attachment"), on_spawn=spawn,
            )
            for offset in attachment_offsets
        ]
        ended_ns, early_stop = observer.settle(planned_start)
        emissions = observer.emissions()
        origin_ns, origin_name = source_module.origin(
            "watch", first_emission_ns=emissions[0] if emissions else planned_start
        )
        for future in history_futures:
            reads.append(relative_read(future.result(), origin_ns, "history"))
        for future in attachment_futures:
            reads.append(relative_read(future.result(), origin_ns, "attachment"))
    return {
        "emissions_ns": emissions,
        "origin_ns": origin_ns,
        "origin": origin_name,
        "ended_ns": ended_ns,
        "early_stop": early_stop,
        "outputs": observer.outputs_ns(),
        "recovery": None,
        "reads": reads,
        "stream": stream,
    }


def measure_recovery(run, fixture, observer, planned_start, stream):
    outage = CONTROLS["outage"]
    with ThreadPoolExecutor(max_workers=1) as executor:
        interrupted = executor.submit(
            workflow.blackout,
            planned_start + outage["start_ms"] * 1_000_000,
            outage["duration_ms"],
            NETWORK,
        )
        ended_ns, early_stop = observer.settle(planned_start)
        blackout_start_ns, restored_ns = interrupted.result()
    emissions = observer.emissions()
    # The model's attempt timeline starts at the first actual source emission
    # (all relative times stay non-negative). Recovery's own cut is restoration,
    # named separately in the observation and the recovery milestones; the
    # blackout interval is retained alongside it.
    origin_ns, origin_name = source_module.origin(
        "watch", first_emission_ns=emissions[0] if emissions else planned_start
    )
    return {
        "emissions_ns": emissions,
        "origin_ns": origin_ns,
        "origin": origin_name,
        "cut": {
            "name": source_module.LINK_RESTORATION,
            "at_ms": source_module.milliseconds(restored_ns, origin_ns),
        },
        "ended_ns": ended_ns,
        "early_stop": early_stop,
        "outputs": observer.outputs_ns(),
        "blackout_start_ns": blackout_start_ns,
        "restored_ns": restored_ns,
        "recovery": True,
        "reads": None,
        "stream": stream,
    }


def apply_measurement(run, attempted, measured):
    origin_ns = measured["origin_ns"]
    ended_ns = measured["ended_ns"]
    emitted_sizes = SOURCE_SIZES[: len(measured["emissions_ns"])]
    times = source_module.source_series(measured["emissions_ns"], emitted_sizes, origin_ns)
    attempted["source"] = times
    raw_outputs = measured["outputs"]
    outputs = []
    for event in raw_outputs:
        ordinal = event["ordinal"]
        outputs.append(
            facts.output_fact(
                at_ms=source_module.milliseconds(event["at_ns"], origin_ns),
                ordinal=ordinal,
                kind=event["kind"],
                bytes_=event["bytes"],
                prefix_bytes=event["prefix_bytes"],
                correct=event["correct"],
                block=event.get("block"),
            )
        )
    attempted["outputs"] = outputs
    attempted["_source_times_ms"] = times["times_ms"]
    deadline_ns = origin_ns + int(DEADLINE_MS * 1_000_000)
    cutoff_ns = min(deadline_ns, ended_ns)
    attempted["observation"] = facts.observation_block(
        started_ns=origin_ns,
        ended_ns=ended_ns,
        cutoff_ns=cutoff_ns,
        origin_ns=origin_ns,
        origin=measured["origin"],
        early_stop=measured["early_stop"],
        cut=measured.get("cut"),
    )
    attempt_id = attempt_slot_id(run, measured["stream"])
    for index, (at_ns, size) in enumerate(zip(measured["emissions_ns"], emitted_sizes)):
        run.emit(
            "fixture_emit",
            ts=at_ns,
            stream=run.capture.token("stream", measured["stream"]),
            attempt=attempt_id,
            block=run.capture.token("block", "text-0"),
            # Host/Core reserve sequence 1 for the start frame; the first
            # visible text increment is sequence 2. Keep the fixture boundary
            # on the same numbering.
            seq_start=index + 2,
            seq_end=index + 2,
            bytes=int(size),
        )
    for event, raw in zip(outputs, raw_outputs, strict=True):
        run.emit(
            "fixture_observe",
            ts=raw["at_ns"],
            attempt=attempt_id,
            ordinal=event["ordinal"],
            record=event["ordinal"],
            kind=event["kind"],
            bytes=event["bytes"],
            prefix_bytes=event["prefix_bytes"],
            correct=event["correct"],
            block=run.capture.token("block", event.get("block") or "text-0"),
        )
    if measured.get("recovery"):
        recovery = workflow.recovery_observation(
            attempted,
            measured["blackout_start_ns"],
            measured["restored_ns"],
            origin_ns,
            outputs,
            emitted_sizes,
            DEADLINE_MS,
        )
        attempted["recovery"] = recovery
        for name, milestone in recovery["milestones"].items():
            run.emit(
                "fixture_recovery",
                attempt=attempt_id,
                milestone=name,
                state=milestone["state"],
            )
    if measured.get("reads"):
        attempted["reads"] = measured["reads"]
        watch_interval = intervals.interval(0.0, attempted["observation"]["cutoff_ms"])
        summary = intervals.exposure(measured["reads"], watch_interval)
        for read in measured["reads"]:
            current = intervals.interval(
                read.get("started_at_ms"), read.get("finished_at_ms")
            )
            read["overlap_ms"] = intervals.overlap_ms(current, watch_interval)
            read["exposure_ms"] = summary["union_ms"]
        attempted["_exposure"] = summary


def attempt_slot_id(run, stream):
    """Attempt identity: the same run-local token every producer derives.

    Fixture events and DSH/Node events must join on one opaque attempt token,
    so the fixture derives it from the source stream instead of inventing a
    second clear identifier.
    """
    return run.capture.token("attempt", stream)


def relative_read(observation, origin_ns, op):
    scheduled_ns = observation.pop("scheduled_ns")
    started_ns = observation.pop("started_ns")
    finished_ns = observation.pop("finished_ns")
    read_id = op + "-" + uuid.uuid4().hex[:10]
    record = {
        "id": read_id,
        "op": op,
        "scheduled_at_ms": source_module.milliseconds(scheduled_ns, origin_ns),
        "started_at_ms": source_module.milliseconds(started_ns, origin_ns),
        "finished_at_ms": source_module.milliseconds(finished_ns, origin_ns),
        "deadline_ms": DEADLINE_MS,
        "pages": int(observation.get("pages") or 0),
        "messages": int(observation.get("messages") or 0),
        "bytes_expected": int(observation.get("bytes_expected") or 0),
        "bytes_verified": int(observation.get("bytes_verified") or 0),
        "physical_bytes": int(observation.get("physical_bytes") or 0),
        "route": observation.get("route") or ("reference" if op == "attachment" else "inline"),
        "outcome": observation.get("outcome") or "product_error",
        "verified": bool(observation.get("verified")),
        "token_repeats": int(observation.get("token_repeats") or 0),
        "corrupt": bool(observation.get("corrupt")),
    }
    if observation.get("error_code"):
        record["error_code"] = observation["error_code"]
    return record


def start_source(dsh, session, stream):
    workload, response = SCENARIO["workload"], SCENARIO["response"]
    duplicate = response["duplicate_final_ms"] - response["final_ms"]
    slot = int(stream.rsplit("-", 1)[-1])
    fault = (CONFIG.get("faults") or {}).get(str(slot), "none")
    return int(
        dsh.control(
            "bench_start",
            session_id=session,
            stream_id=stream,
            updates=workload["updates"],
            gap_ms=workload["gap_ms"],
            start_ms=response["start_ms"],
            final_ms=response["final_ms"],
            delay_ms=response["delay_ms"],
            duplicate_after_ms=duplicate if fault == "duplicate" else None,
            fault=fault,
        )
    )


class Observer:
    """Reads CLI stdout and records one monotonic time per public record."""

    def __init__(self, run, device, scenario, session, stream, slot):
        self.run = run
        self.stream = stream
        self.slot = slot
        self.buffer = bytearray()
        self.armed = False
        self.preview = b""
        self.terminal_ns = None
        self.terminals = 0
        self.preview_after_terminal = False
        self.outputs = []
        self.ordinal = 0
        self.content = None
        self.host = scenario.host
        self.session = session
        self.attached = False
        lifetime = "cli-" + uuid.uuid4().hex[:16]
        environment = {
            **device.env,
            "WHALEREIN_LOG": "error",
            "NO_COLOR": "1",
            **run.capture.environment("cli", lifetime),
        }
        self.process = Process(
            device.args(
                "session", "watch", session, "--host", self.host, "--timeout",
                str(int(DEADLINE_MS / 1000) + 60),
            ),
            scenario.directory,
            environment,
            control=True,
        )
        self.pending = None
        if run.capture.trace:
            entry = run.clock.register("cli", pid=self.process.child.pid)
            self.pending = {
                "entry": entry,
                "outlet": run.capture.outlet_path("cli", lifetime),
                "before_ns": clock_module.monotonic_ns(),
                "after_ns": None,
            }
            run.pending_clocks.append(self.pending)
        else:
            run.clock.register("cli", pid=self.process.child.pid)

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
            observed_at = clock_module.monotonic_ns()
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
            visible = text.encode()
            if self.terminals:
                self.preview_after_terminal = True
            if event.get("replace"):
                candidate = visible
                self.preview = visible
                output_kind = "replace"
            else:
                candidate = self.preview + visible
                self.preview = candidate
                output_kind = "preview"
            prefix = self.prefix_of(candidate)
            correct = prefix == len(candidate)
            if not correct:
                output_kind = "wrong"
            self.record(at_ns, output_kind, len(visible), prefix, correct, "text-0")
        elif kind == "reset":
            self.preview = b""
            self.record(at_ns, "reset", 0, 0, True, "text-0")
        elif kind == "message":
            origin = event.get("origin") or {}
            if event.get("role") != "ASSISTANT" or (origin and origin.get("attempt_id") != self.stream):
                return
            content = workflow.text_of(event)
            correct = content == FINAL_TEXT
            duplicate = self.terminals > 0
            self.terminals += 1
            if self.terminal_ns is None:
                self.terminal_ns = at_ns
            if not duplicate:
                self.content = content
            self.record(
                at_ns,
                "duplicate" if duplicate else "terminal",
                len(content.encode()),
                len(content.encode()) if correct else 0,
                correct,
                "text-0",
            )

    def prefix_of(self, candidate):
        prefix = 0
        for actual, expected in zip(candidate, FINAL_BYTES):
            if actual != expected:
                break
            prefix += 1
        return prefix

    def record(self, at_ns, kind, bytes_, prefix, correct, block):
        self.ordinal += 1
        self.outputs.append(
            {
                "at_ns": at_ns,
                "ordinal": self.ordinal,
                "kind": kind,
                "bytes": bytes_,
                "prefix_bytes": prefix,
                "correct": bool(correct),
                "block": block,
            }
        )

    def settle(self, planned_start):
        """Observe through the terminal window or the horizon."""
        horizon = planned_start + (
            DEADLINE_MS + OBSERVATION_WINDOW_MS + SOURCE_START_ALLOWANCE_MS
        ) * 1_000_000
        early_stop = "deadline"
        while clock_module.monotonic_ns() < horizon:
            now = clock_module.monotonic_ns()
            if self.terminal_ns is not None and now >= self.terminal_ns + OBSERVATION_WINDOW_MS * 1_000_000:
                early_stop = "terminal_window"
                break
            if self.process.child.poll() is not None:
                early_stop = "error"
                break
            self.read(0.02)
        return clock_module.monotonic_ns(), early_stop

    def emissions(self):
        values = self.run.session.dsh.process.control(
            "bench_text_times_ns", session_id=self.session
        )
        return [int(value) for value in values]

    def outputs_ns(self):
        return list(self.outputs)

    def close(self):
        self.process.close()
        if self.pending is not None:
            self.pending["after_ns"] = clock_module.monotonic_ns()


def close_device(device):
    directory = getattr(device, "instance_dir", None)
    if directory is None:
        return
    import shutil

    shutil.rmtree(directory, ignore_errors=True)


def capture_trace(run):
    """Drain outlets, validate/merge, and record trace metadata + coverage."""
    if MODE != "trace" or run.manifest is None:
        run.trace = facts.trace_block(mode=MODE)
        return
    started = time.process_time()
    outlet_directory = REPORTS / "outlet"
    outlet_files = {}
    if outlet_directory.is_dir():
        for path in sorted(outlet_directory.glob("*.ndjson")):
            outlet_files[str(path)] = path.name.split("-", 1)[0]
    emit_sampler_events(run)
    run.outlet_flush()
    _private_staging(outlet_directory)
    merged = events_module.merge(
        outlet_files,
        run.manifest,
        run_id=run.capture.run_id,
        cap_bytes=TRACE_CAP_BYTES,
        reserve_bytes=TRACE_RESERVE_BYTES,
    )
    finalize_pending_clocks(run, merged["events"])
    summary = events_module.write_events(REPORTS / "events.ndjson", merged["events"])
    run.exporter_cpu_ms = round((time.process_time() - started) * 1000, 3)
    attempt_coverage = per_attempt_coverage(run, merged["events"])
    for attempt in run.attempts:
        attempt["trace"] = {
            "coverage": attempt_coverage.get(attempt["id"], {}),
            "loss": {"events": merged["losses"].get("outlet_records", 0)},
        }
    run.trace = facts.trace_block(
        path="events.ndjson",
        checksum=summary["checksum"],
        bytes_=summary["bytes"],
        cap_bytes=TRACE_CAP_BYTES,
        complete=merged["complete"],
        losses=merged["losses"],
        coverage=merged["coverage"],
        events_path="events.ndjson",
        outlets={Path(path).name: value for path, value in merged["outlets"].items()},
        mode=MODE,
    )
    if not merged["complete"] and run.state == "complete":
        run.fail("trace_incomplete", state="incomplete")
    if merged["truncated"]:
        run.fail("trace_byte_cap", state="incomplete")


def emit_sampler_events(run):
    """Record sampled socket windows as typed fixture facts before draining."""
    if run.sampler is None or run.outlet is None:
        return
    summary = run.sampler.summary()
    for window in summary.get("windows", []):
        state = window["state"]
        if state not in ("sampled", "idle", "unsupported", "replaced", "dropped"):
            state = "sampled"
        payload = {"role": window["role"], "generation": int(window["generation"]), "state": state}
        if window.get("rtt_us") is not None:
            payload["rtt_us"] = int(window["rtt_us"])
        if window.get("retransmits") is not None:
            payload["retransmits"] = int(window["retransmits"])
        if window.get("queue_bytes") is not None:
            payload["queue_bytes"] = int(window["queue_bytes"])
        if window.get("gap_ms") is not None:
            payload["gap_ms"] = int(window["gap_ms"])
        run.emit("fixture_socket_sample", **payload)


def finalize_pending_clocks(run, events):
    """Bracket short-lived producer readings with the fixture clock."""
    by_outlet = {}
    for event in events:
        by_outlet.setdefault(event.get("_outlet"), []).append(event["ts"])
    for pending in run.pending_clocks:
        after = pending.get("after_ns") or clock_module.monotonic_ns()
        readings = by_outlet.get(pending["outlet"]) or []
        entry = pending["entry"]
        if pending["outlet"] not in by_outlet:
            entry["reason"] = "reading_unavailable"
            continue
        ok = bool(pending["before_ns"] <= min(readings) <= max(readings) <= after)
        entry["verified"] = ok
        entry["reason"] = None if ok else "reading_outside_bracket"
        if ok:
            entry.pop("reason", None)


def per_attempt_coverage(run, events):
    """Coverage per attempt from attempt-scoped typed events only."""
    manifest = run.manifest
    relevant = {}
    for event in events:
        attempt = event["data"].get("attempt")
        if attempt:
            relevant.setdefault(attempt, set()).add(event["event"])
    paths = []
    workflow_name = RUN_CONFIG["workflow"]
    if workflow_name in ("watch", "mixed-load"):
        paths.append("preview")
    if workflow_name == "recovery":
        paths.extend(["preview", "recovery"])
    if workflow_name == "mixed-load":
        paths.extend(["bulk", "object"])
    result = {}
    for attempt in run.attempts:
        observed = relevant.get(attempt["id"], set())
        coverage = {}
        for path in dict.fromkeys(paths):
            required = manifest["coverage_paths"][path]
            coverage[path] = {
                "required": list(required),
                "observed": [name for name in required if name in observed],
                "complete": set(required) <= observed,
                "inapplicable": {},
            }
        result[attempt["id"]] = coverage
    return result


def document_for(run):
    ensure_all_slots(run)
    reconciliation = slots.reconcile(run.schedule, run.attempts)
    if not reconciliation["complete"] and "slot_reconciliation_failed" not in run.reasons:
        run.fail("slot_reconciliation_failed")
    sampler_summary = None
    resources = None
    if run.sampler is not None:
        sampler_summary = run.sampler.summary(
            exporter_cpu_ms=run.exporter_cpu_ms,
        )
        if not sampler_summary.get("access"):
            sampler_summary["access"] = {"available": False, "reason": "unsupported"}
        resources = {
            "cpu_time_ms": sampler_module.fixture_cpu_ms(),
            "peak_rss_bytes": sampler_module.proc_peak_rss(),
            "sampler_cpu_ms": sampler_summary.get("sampler_cpu_ms"),
            "exporter_cpu_ms": run.exporter_cpu_ms,
            "limits": sampler_module.parse_cgroup_limits(
                _read_text("/sys/fs/cgroup/cpu.max"),
                _read_text("/sys/fs/cgroup/memory.max"),
            ),
            "profiler": sampler_summary.get("profiler"),
        }
    return facts.document(
        run=facts.run_block(
            capture=run.capture,
            configuration=RUN_CONFIG,
            started_at=run.started_at,
            fingerprint=fingerprint_block(run),
        ),
        scenario=SCENARIO,
        environment=environment_for(run, sampler=sampler_summary, resources=resources),
        schedule=run.schedule,
        attempts=run.attempts,
        trace=run.trace or facts.trace_block(mode=MODE),
        status=facts.status_block(
            state=run.state, reasons=list(run.reasons), cleanup=run.cleanup
        ),
    )


def _private_staging(directory):
    """Keep the drained staging owner-only, even after a failed export."""
    try:
        directory.chmod(0o700)
    except OSError:
        pass
    if not directory.is_dir():
        return
    for path in directory.glob("*.ndjson"):
        try:
            path.chmod(0o600)
        except OSError:
            pass


def fallback_document(run):
    """Publish the facts already known when document assembly itself fails."""
    ensure_all_slots(run)
    return facts.document(
        run=facts.run_block(
            capture=run.capture,
            configuration=RUN_CONFIG,
            started_at=run.started_at,
            fingerprint=fingerprint_block(run),
        ),
        scenario=SCENARIO,
        environment=environment_for(run),
        schedule=run.schedule,
        attempts=run.attempts,
        trace=run.trace or facts.trace_block(mode=MODE),
        status=facts.status_block(
            state=run.state, reasons=list(run.reasons), cleanup=run.cleanup
        ),
    )


def ensure_all_slots(run):
    """Emit an explicit not_started fact for every unstarted planned slot."""
    seen = {attempt["slot"] for attempt in run.attempts}
    for slot in run.schedule["slots"]:
        if slot["slot"] not in seen:
            run.attempts.append(facts.not_started(slot))
    run.attempts.sort(key=lambda attempt: attempt["slot"])


def environment_for(run, sampler=None, resources=None):
    clock = run.clock.result()
    return facts.environment_block(
        artifacts=artifacts(),
        limits=environment_limits(),
        versions=versions(),
        fixture={
            "name": "FakeDSH",
            "script": SCENARIO["id"],
            "transport": "unix",
            "fingerprint": run.fixture,
        },
        clock=clock,
        capture_origin_ns=run.capture_origin_ns,
        network_probe=run.network_probe,
        network_counters=run.network_counters,
        sampler=sampler,
        resources=resources,
    )


def fingerprint_block(run):
    values = artifacts()
    return {
        "build": {
            "revision": os.environ.get("WHALEREIN_BUILD_REVISION", "unavailable"),
            "fingerprint": values.get("cli", "unavailable"),
        },
        "fixture": {"name": "FakeDSH", "fingerprint": run.fixture},
        "schemas": {"case": facts.SCHEMA_VERSION, "event": 1, "workload": facts.SCENARIO_VERSION},
        "platform": {
            "os": platform.system().lower(),
            "arch": platform.machine(),
            "kernel": platform.release(),
        },
        "limits": environment_limits(),
        "clock": {
            "source": clock_module.CLOCK_SOURCE,
            "unit": clock_module.CLOCK_UNIT,
            "verified": bool(run.clock.verified),
        },
        "sampling": {
            "socket_hz": round(1000 / SAMPLER_INTERVAL_MS, 3),
            "runtime_hz": round(1000 / SAMPLER_INTERVAL_MS, 3),
        },
    }


def environment_limits():
    parsed = sampler_module.parse_cgroup_limits(
        _read_text("/sys/fs/cgroup/cpu.max"), _read_text("/sys/fs/cgroup/memory.max")
    )
    return {
        "cpus": parsed["cpus"] or os.cpu_count(),
        "memory_bytes": parsed["memory_bytes"] or total_memory(),
    }


def versions():
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "node": node_version(),
        "cli": executable_version(),
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
        (
            "tests/integration/environment.py",
            "tests/integration/fixtures/plugin.mjs",
            "tests/integration/fixtures/adapter.mjs",
            "tests/integration/client_environment.py",
            "tools/repo/src/repo/benchmark/model.py",
            "tools/repo/src/repo/benchmark/analysis.py",
            "contracts/latency/observation.json",
            "contracts/latency/vectors.json",
        ),
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
            ["node", "--version"], check=False, capture_output=True, text=True, timeout=10
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


def _read_text(path):
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def _private(path):
    try:
        Path(path).chmod(0o644)
    except OSError:
        pass


def stable_reason(error):
    name = type(error).__name__
    text = str(error).strip()
    if text and len(text) <= 120 and all(character.isprintable() for character in text):
        return f"{name}:{text}"
    return name


def exit_code(run):
    if run.state != "complete":
        return 1
    for attempt in run.attempts:
        if attempt["phase"] != "measured":
            continue
        if attempt["status"] in ("setup_failed", "interrupted", "not_started"):
            return 1
        for read in attempt.get("reads", []):
            if read.get("outcome") != "completed":
                return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())