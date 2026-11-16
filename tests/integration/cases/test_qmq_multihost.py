"""QMQ-08: same-named sessions on two Hosts never leak another Host's model or queue."""

import pytest

from environment import http
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_same_named_sessions_require_explicit_host_for_model_queue_and_questions(scenario, paired_device):
    """Unqualified model, queue and question reads refuse an ambiguous session instead of picking a Host."""
    directory = scenario.directory / "qmq-second-native"
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = "host-integration-2"
    other.http_port = 3111
    other.env = scenario.env.copy()
    try:
        other.start_native()
        invitation = http("/v1/pairing/invitations", {"hostId": other.host}, scenario.admin)
        paired_device.call("pair", invitation["pairingUri"])
        assert scenario.request("test-enable-create")["enabled"]
        assert other.request("test-enable-create")["enabled"]
        shared = "qmq-shared-session"
        assert paired_device.call("session-create", "--host", scenario.host, "--session-id", shared)["lifecycle_receipt"]["session_id"] == shared
        assert paired_device.call("session-create", "--host", other.host, "--session-id", shared)["lifecycle_receipt"]["session_id"] == shared

        # An unqualified read is ambiguous: it must not silently pick either Host.
        for command in (
            ("model", "get", "--session", shared),
            ("queue", "list", "--session", shared),
            ("questions", "--session", shared),
        ):
            code, values = paired_device.invoke(*command, success=False)
            assert code == 8, (command, values)
            assert values[-1]["error"]["code"] == "conflict", (command, values)
            assert scenario.host in values[-1]["error"]["message"]
            assert other.host in values[-1]["error"]["message"]

        # Each explicitly routed read observes only its own Host.
        first_catalog = scenario.request("model-catalog")
        second_catalog = other.request("model-catalog")
        first_group = first_catalog["groups"][0]
        second_group = second_catalog["groups"][0]
        assert len(first_group["models"]) >= 2 and len(second_group["models"]) >= 2
        first_selection = {"provider": first_group["id"], "model": first_group["models"][0]["id"]}
        second_selection = {"provider": second_group["id"], "model": second_group["models"][1]["id"]}
        scenario.request("model-select-local", sessionId=shared, selection=first_selection)
        other.request("model-select-local", sessionId=shared, selection=second_selection)
        first_model = paired_device.call("--host", scenario.host, "model", "get", "--session", shared)
        second_model = paired_device.call("--host", other.host, "model", "get", "--session", shared)
        assert first_model["next"]["model"] == first_selection["model"]
        assert second_model["next"]["model"] == second_selection["model"]
        assert first_model["next"]["model"] != second_model["next"]["model"]
        # Queue and question reads with an explicit Host answer for that Host only.
        for host in (scenario.host, other.host):
            queue = paired_device.call("--host", host, "queue", "list", "--session", shared)
            assert queue["session_id"] == shared
            assert isinstance(queue["items"], list)
            assert isinstance(paired_device.call("--host", host, "questions", "--session", shared)["batches"], list)
    finally:
        other.close()
