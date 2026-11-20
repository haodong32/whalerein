"""Public integration fixtures; cases never start Docker."""

import os
import socket
import subprocess
import threading
import uuid
from contextlib import ExitStack
from pathlib import Path

import pytest
from environment import (
    BROKER_PASSWORD,
    INTERNAL_TOKEN,
    ROOT,
    Process,
    Scenario,
    device_keyring,
    start_connect_policy_stub,
    wait,
)
from sharding import parse_shard, partition

pytest_plugins = ("integration_report",)


def pytest_addoption(parser):
    parser.addoption("--partial-scope", action="store_true")
    parser.addoption("--only", help="Select a pytest case by substring")
    parser.addoption(
        "--shard",
        metavar="INDEX/COUNT",
        help="run only the 0-based deterministic hash shard INDEX of COUNT",
    )


def pytest_collection_modifyitems(config, items):
    from integration_report import validate_descriptions

    try:
        validate_descriptions(items)
    except ValueError as error:
        raise pytest.UsageError(str(error)) from error
    only = config.getoption("--only")
    if only is not None and not only.strip():
        raise pytest.UsageError("--only requires nonempty text")
    shard = config.getoption("--shard")
    if shard is not None:
        try:
            index, count = parse_shard(shard)
        except ValueError as error:
            raise pytest.UsageError(str(error)) from error
    selected = []
    deselected = []
    for item in items:
        (
            selected
            if not only or only in item.nodeid
            else deselected
        ).append(item)
    if len({item.nodeid for item in items}) != len(items):
        raise pytest.UsageError("Duplicate integration case identity")
    sharded = bool(shard is not None)
    if not selected:
        raise pytest.UsageError("No integration cases match the selection")
    if sharded:
        # Sharding applies after path/--only selection and distributes the remaining
        # selection; an empty shard is a legitimate result when COUNT exceeds
        # the number of selected cases.
        selected = partition(selected, index, count)
        selected_ids = {item.nodeid for item in selected}
        deselected = [item for item in items if item.nodeid not in selected_ids]
    config.hook.pytest_deselected(items=deselected)
    items[:] = selected


@pytest.fixture(scope="session", autouse=True)
def container_only():
    if os.environ.get("WHALEREIN_INTEGRATION_CONTAINER") != "1":
        pytest.fail(
            "Run integration cases with ./repo test integration", pytrace=False
        )


@pytest.fixture
def tmp_path(tmp_path_factory):
    # The disposable container owns cleanup. Avoid pytest's outcome-dependent
    # tmp_path finalizer, which can fail if setup is interrupted before a report.
    return tmp_path_factory.mktemp("case")


@pytest.fixture(scope="session")
def postgres(tmp_path_factory):
    from integration_report import progress

    progress({"event": "detail", "description": "Preparing PostgreSQL"})
    directory = tmp_path_factory.mktemp("postgres")
    directory.chmod(0o755)
    # pytest temporary ancestors must be traversable by the database account.
    for parent in list(directory.parents)[:2]:
        parent.chmod(0o755)
    data = directory / "data"
    data.mkdir()
    subprocess.run(["chown", "postgres:postgres", str(data)], check=True, timeout=10)
    prefix = ["runuser", "-u", "postgres", "--"]
    subprocess.run(
        [*prefix, "/usr/lib/postgresql/18/bin/initdb", "-D", str(data), "-A", "trust"],
        check=True,
        capture_output=True,
        timeout=30,
    )
    process = Process(
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
    try:
        wait(
            lambda: (
                subprocess.run(
                    ["pg_isready", "-h", "127.0.0.1", "-p", "55432"],
                    capture_output=True,
                    check=False,
                    timeout=5,
                ).returncode
                == 0
            )
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
            timeout=10,
        )
        yield
    finally:
        process.close()


class Broker:
    """One disposable Mosquitto 2.1.4 broker per container, supervised across cases.

    The broker is transport only: persistence is disabled, its plugin state is
    in memory, and the Access service account authenticates locally against the
    plugin password file. External principals are admitted through Access's
    HTTP callback, so a crash is recoverable; the next case restarts it instead
    of cascading failures.
    """

    url = "mqtt://127.0.0.1:1883"

    def __init__(self, directory):
        self.directory = Path(directory)
        self.plugindir = self.directory / "broker"
        self.config = self._config_text()
        self.process = None
        self.restarts = 0

    def _config_text(self):
        self.plugindir.mkdir(parents=True, exist_ok=True)
        # pytest temporary ancestors are private by default, but Mosquitto
        # drops to the unprivileged nobody account before loading the plugin.
        # Make the generated credential files traversable within the disposable
        # container, as the PostgreSQL fixture already does for its data tree.
        for directory in [self.plugindir, *list(self.plugindir.parents)[:3]]:
            try:
                directory.chmod(0o755)
            except OSError:
                pass
        password_file = self.plugindir / "access-password"
        token_file = self.plugindir / "internal-token"
        password_file.write_text(BROKER_PASSWORD)
        token_file.write_text(INTERNAL_TOKEN)
        return "\n".join(
            [
                "# Generated per-broker config for the integration container.",
                "listener 1883 127.0.0.1",
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

    def start(self):
        if self.process is not None:
            return
        config = self.plugindir / "mosquitto.conf"
        config.write_text(self.config)
        from integration_report import progress

        progress({"event": "detail", "description": "Waiting for broker readiness"})
        self.process = Process(
            ["/usr/local/sbin/mosquitto", "-c", str(config)], self.directory
        )

        def ready():
            with socket.create_connection(("127.0.0.1", 1883), timeout=1):
                return True

        wait(ready, process=self.process)

    def ensure_started(self):
        """Restart a broker that exited, then wait for readiness."""
        if self.process is not None and self.process.child.poll() is not None:
            self.process.close()
            self.process = None
            self.restarts += 1
            from integration_report import progress

            progress(
                {
                    "event": "detail",
                    "description": f"Restarting broker after exit (restart {self.restarts})",
                }
            )
        self.start()

    def close(self):
        if self.process is not None:
            self.process.close()
            self.process = None


@pytest.fixture
def broker_control(tmp_path, request):
    policy_stub = None
    if request.node.path.name == "test_transport.py":
        case = getattr(request.node, "callspec", None)
        if case is not None and case.params.get("case") in ("node", "boundaries"):
            # The Node.js transport case has no Access process; the plugin's
            # deferred CONNECT callback is served by a loopback policy stub.
            policy_stub = start_connect_policy_stub()
    fixture = Broker(tmp_path)
    try:
        yield fixture
    finally:
        fixture.close()
        if policy_stub is not None:
            policy_stub.close()


@pytest.fixture(scope="session")
def shared_broker(tmp_path_factory):
    """Opt-in container-warm broker shared by scenario cases.

    Clean-start MQTT clients and non-retained publishes make a warm broker
    reusable in principle, but case isolation must be proven per feature group
    before this becomes the default. Transport cases still stop it first.
    """
    # Shared by default; set WHALEREIN_INTEGRATION_SHARED_BROKER=0 to keep a
    # broker per case (for example while investigating broker-state isolation).
    if os.environ.get("WHALEREIN_INTEGRATION_SHARED_BROKER", "1") != "1":
        yield None
        return
    directory = tmp_path_factory.mktemp("shared-broker")
    fixture = Broker(directory)
    yield fixture
    fixture.close()


@pytest.fixture
def broker(broker_control, shared_broker):
    """Direct broker consumers get a started instance and its MQTT URL."""
    if shared_broker is not None:
        shared_broker.close()
    broker_control.ensure_started()
    yield broker_control.url


@pytest.fixture
def scenario(tmp_path, postgres, broker_control, shared_broker, request):
    schema = "case_" + uuid.uuid4().hex
    command = [
        "psql",
        "-h",
        "127.0.0.1",
        "-p",
        "55432",
        "-U",
        "postgres",
        "-d",
        "whalerein_test",
        "-v",
        "ON_ERROR_STOP=1",
        "-c",
    ]
    subprocess.run(
        [*command, f"CREATE SCHEMA {schema}"],
        check=True,
        capture_output=True,
        timeout=10,
    )
    native = request.node.get_closest_marker("native-dsh") is not None
    if native:
        from native_environment import NativeScenario

        value = NativeScenario(tmp_path, schema, timed_questions=request.node.get_closest_marker("timed_questions") is not None, short_approvals=request.node.get_closest_marker("short_approvals") is not None, debug_logging=request.node.get_closest_marker("privacy_debug_logging") is not None, wrapper_launcher=request.node.get_closest_marker("wrapper_launcher") is not None, external_launcher=request.node.get_closest_marker("external_launcher") is not None, reasoning=request.node.get_closest_marker("native_reasoning") is not None, compactable=request.node.get_closest_marker("native_compaction") is not None)
        if "host_mqtt_relay" in request.fixturenames:
            value.mqtt_port = request.getfixturevalue("host_mqtt_relay").port
    else:
        value = Scenario(tmp_path, schema)
    tls_marker = request.node.get_closest_marker("tls")
    tls_broker = None
    if not native and tls_marker is not None:
        from tls_environment import TlsBroker, certificate

        kind = tls_marker.args[0] if tls_marker.args else "server"
        value.configure_tls(certificate(kind)["ca"])
        tls_broker = TlsBroker(tmp_path / "tls-broker", cert=kind)
    # Journey cases restart the case-owned broker or rotate the case TLS
    # listener; expose the fixture instances without changing their lifecycle.
    value.tls_broker = tls_broker
    if request.node.get_closest_marker("privacy_access_info_logging") is not None:
        config = value.config.read_text()
        assert config.count("log:\n  level: error") == 1
        value.config.write_text(config.replace("log:\n  level: error", "log:\n  level: info", 1))
    policy = request.node.get_closest_marker("attachment_policy")
    if policy is not None and not native:
        value.attachment_policy = dict(policy.kwargs)
    try:
        from integration_report import progress

        progress(
            {
                "event": "detail",
                "description": "Preparing native DSH and local model"
                if native
                else "Preparing FakeDSH, Access and plugin",
            }
        )
        broker = shared_broker if shared_broker is not None else broker_control
        value.broker = broker
        if native:
            # Access first: its internal callback endpoint must answer the
            # plugin's deferred CONNECT requests. Waiting for /readyz here
            # would deadlock until the broker exists.
            value.start_access_process()
            broker.ensure_started()
            value.wait_access_ready()
            value.start_native()
        else:
            value.start_dsh()
            if tls_broker is not None:
                tls_broker.start()
            else:
                broker.ensure_started()
            value.start_access_process()
            value.wait_access_ready()
            value.start_plugin()
        yield value
    finally:
        from integration_report import progress

        progress({"event": "detail", "description": "Cleaning up case environment"})
        try:
            value.close()
        finally:
            if tls_broker is not None:
                tls_broker.close()
            subprocess.run(
                [*command, f"DROP SCHEMA {schema} CASCADE"],
                check=True,
                capture_output=True,
                timeout=10,
            )


@pytest.fixture(autouse=True)
def device_keyring_scope(tmp_path_factory):
    """Give Device instances a lazy, case-owned Linux Secret Service."""
    from client_environment import isolated_keyring

    with ExitStack() as resources:
        lock = threading.Lock()
        environment = None

        def keyring():
            nonlocal environment
            with lock:
                if environment is None:
                    directory = tmp_path_factory.mktemp("device-keyring")
                    environment = resources.enter_context(isolated_keyring(directory))
                return environment

        token = device_keyring.set(keyring)
        try:
            yield
        finally:
            device_keyring.reset(token)


@pytest.fixture
def paired_device(scenario):
    return scenario.pair()


@pytest.fixture
def paired_client(scenario):
    """Connect a fresh CLI instance through the selected Standalone TOML."""
    from client_environment import CutoverClient, isolated_keyring
    from environment import http

    with isolated_keyring(scenario.directory) as keyring:
        client = CutoverClient(scenario.directory, "selected-client", keyring)
        invitation = http(
            "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
        )["pairingUri"]
        client.identity = client.call(
            "host", "connect", "--invite-stdin", "--yes", input_text=invitation
        )
        yield client


@pytest.fixture
def session_id(scenario):
    return scenario.dsh.create_session()


@pytest.fixture
def pending_approval(scenario, session_id):
    return scenario.dsh.request_approval(session_id)
