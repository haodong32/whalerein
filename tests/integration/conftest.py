"""Public integration fixtures; cases never start Docker."""

import os
import subprocess
import uuid
from pathlib import Path

import pytest
from environment import ROOT, Process, Scenario, wait

pytest_plugins = ("integration_report",)


def pytest_addoption(parser):
    parser.addoption(
        "--group",
        action="append",
        choices=["protocol", "storage", "transport", "chain"],
    )
    parser.addoption("--only", help="Select a pytest case by substring")


def pytest_collection_modifyitems(config, items):
    from integration_report import validate_descriptions

    try:
        validate_descriptions(items)
    except ValueError as error:
        raise pytest.UsageError(str(error)) from error
    groups = config.getoption("--group") or []
    only = config.getoption("--only")
    if only is not None and not only.strip():
        raise pytest.UsageError("--only requires nonempty text")
    selected = []
    deselected = []
    for item in items:
        memberships = {marker.name for marker in item.iter_markers()}
        (
            selected
            if (not groups or memberships.intersection(groups))
            and (not only or only in item.nodeid)
            else deselected
        ).append(item)
    if len({item.nodeid for item in items}) != len(items):
        raise pytest.UsageError("Duplicate integration case identity")
    if not selected:
        raise pytest.UsageError("No integration cases match the selection")
    config.hook.pytest_deselected(items=deselected)
    items[:] = selected


@pytest.fixture(scope="session", autouse=True)
def container_only():
    if os.environ.get("WHALEREIN_INTEGRATION_CONTAINER") != "1":
        pytest.fail("Run integration cases with ./repo test --suite integration", pytrace=False)


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


@pytest.fixture
def broker(tmp_path, request):
    transport = (
        request.node.path.name == "test_transport.py"
        and request.node.callspec.params["case"] != "routes"
    )
    source = ROOT / (
        "deploy/compose/emqx/transport-test.conf"
        if transport
        else "deploy/compose/emqx/emqx.conf"
    )
    config = source.read_text().replace("http://access:8081", "http://127.0.0.1:18081")
    config = config.replace(
        'data_dir = "/opt/emqx/data"', f'data_dir = "{tmp_path / "broker"}"'
    )
    config += "\nauthorization.cache.enable = false\n"
    Path("/opt/emqx/etc/emqx.conf").write_text(config)
    Path("/opt/emqx/etc/api-keys.csv").write_text(
        (ROOT / "deploy/compose/emqx/api-keys.csv").read_text()
    )
    from integration_report import progress

    progress({"event": "detail", "description": "Waiting for broker readiness"})
    process = Process(["/opt/emqx/bin/emqx", "foreground"], tmp_path)
    try:
        wait(
            lambda: (
                subprocess.run(
                    ["/opt/emqx/bin/emqx", "ctl", "status"],
                    capture_output=True,
                    check=False,
                    timeout=5,
                ).returncode
                == 0
            ),
            process=process,
        )
        yield "mqtt://127.0.0.1:1883"
    finally:
        process.close()


@pytest.fixture
def scenario(tmp_path, postgres, broker):
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
        [*command, f"CREATE SCHEMA {schema}"], check=True, capture_output=True, timeout=10
    )
    value = Scenario(tmp_path, schema)
    try:
        from integration_report import progress

        progress(
            {"event": "detail", "description": "Preparing FakeDSH, Access and plugin"}
        )
        value.start_dsh()
        value.start_access()
        value.start_plugin()
        yield value
    finally:
        from integration_report import progress

        progress({"event": "detail", "description": "Cleaning up case environment"})
        try:
            value.close()
        finally:
            subprocess.run(
                [*command, f"DROP SCHEMA {schema} CASCADE"],
                check=True,
                capture_output=True,
                timeout=10,
            )


@pytest.fixture
def paired_device(scenario):
    return scenario.pair()


@pytest.fixture
def session_id(scenario):
    return scenario.dsh.create_session()


@pytest.fixture
def pending_approval(scenario, session_id):
    return scenario.dsh.request_approval(session_id)
