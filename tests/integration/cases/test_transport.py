"""Native integration assertions driven by pytest in the same container."""

import os
import re
import subprocess

import pytest
from environment import BROKER_PASSWORD, INTERNAL_TOKEN, ROOT

pytestmark = pytest.mark.transport


@pytest.mark.parametrize(
    "case",
    [
        pytest.param(
            "routes",
            marks=pytest.mark.description(
                "When the Go MQTT adapter and a host connect through the Mosquitto line, requests, results, events, presence and transient replies reach their intended handlers."
            ),
        ),
        pytest.param(
            "logical",
            marks=pytest.mark.description(
                "When two MiB logical messages cross Go, Node.js and Rust through a packet-limited broker, complete requests, results and receipted events are delivered."
            ),
        ),
        pytest.param(
            "identity",
            marks=pytest.mark.description(
                "When the CLI exchanges large encrypted snapshots and live events, revocation denies the old token and removes grants and peers; re-pairing after revocation or config loss retains key identity with one active grant and peer."
            ),
        ),
        pytest.param(
            "node",
            marks=pytest.mark.description(
                "When the Node.js MQTT adapter publishes a two MiB result, bounded MQTT frames deliver the complete body without reconnecting."
            ),
        ),
        pytest.param(
            "boundaries",
            marks=pytest.mark.description(
                "Payloads of one byte and at 60 KiB plus or minus one cross the real broker intact, with every wire packet inside the packet budget."
            ),
        ),
    ],
)
def test_native_transport(case, postgres, broker):
    """Passes the selected real-broker transport regression without skipped assertions."""
    settings = {
        **os.environ,
        "WHALEREIN_TEST_MQTT_URL": broker,
        "WHALEREIN_BROKER_MQTT_URL": broker,
        "WHALEREIN_TEST_ACCESS_PASSWORD": BROKER_PASSWORD,
        "WHALEREIN_TEST_HOST_PASSWORD": "host-pass",
        "WHALEREIN_TEST_INTERNAL_PORT": "18081",
        "WHALEREIN_TEST_BROKER_PASSWORD": BROKER_PASSWORD,
        "WHALEREIN_TEST_INTERNAL_TOKEN": INTERNAL_TOKEN,
    }
    commands = {
        "routes": [
            "/workspace/.verification/bin/broker-tests",
            "-test.run=^TestMQTTIntegrationRoutesRequestsResultsEventsAndPresenceThroughMosquitto$",
            "-test.v",
        ],
        "logical": [
            "/workspace/.verification/bin/broker-tests",
            "-test.run=^TestMQTTLogicalTransportCarriesLargeMessagesBetweenGoNodeAndRust$",
            "-test.v",
        ],
        "identity": [
            "/workspace/.verification/bin/server-tests",
            "-test.run=^TestIdentityAndEncryptedTransportAgainstIsolatedStack$",
            "-test.v",
        ],
        "node": [
            "node",
            "--test",
            "tests/integration/drivers/transport/mqtt-large-message.test.js",
        ],
        "boundaries": [
            "node",
            "--test",
            "tests/integration/drivers/transport/mqtt-boundary-message.test.js",
        ],
    }
    directory = ROOT / (
        "access/cmd/server"
        if case == "identity"
        else "access/internal/broker"
        if case in ("logical", "routes")
        else "."
    )
    result = subprocess.run(
        commands[case],
        cwd=directory,
        env=settings,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    exit_code = result.returncode
    locations = sorted(set(re.findall(r"[A-Za-z_]+_test.go:\d+", result.stdout)))
    # Native payloads remain private; only explicitly typed count/code diagnostics
    # can supplement the source locations in a failing assertion.
    safe = re.search(r"large live delivery failed: exit=(?:<nil>|exit status [0-9]+) bytes=[0-9]+ code=[a-z_]*", result.stdout)
    hint = "; " + safe.group(0) if safe else ""
    assert exit_code == 0, f"Native {case} integration failed at " + ", ".join(locations) + hint
    if "SKIP" in result.stdout:
        raise AssertionError("Required native integration was skipped")
