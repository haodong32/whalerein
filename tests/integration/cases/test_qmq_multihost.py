"""QMQ-08: same-named sessions on two Hosts never leak another Host's model or queue."""

import pytest

from environment import http
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_same_named_sessions_use_scoped_model_queue_and_question_reads(scenario, paired_device):
    """Explicit reads stay with their selected Host and implicit reads use the default."""
    directory = scenario.directory / "qmq-second-native"
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = "host-integration-2"
    other.http_port = 3111
    other.env = scenario.env.copy()
    try:
        other.start_native()
        invitation = http("/v1/pairing/invitations", {"hostId": other.host}, scenario.admin)
        paired_device.pair(invitation["pairingUri"])
        assert scenario.request("test-enable-create")["enabled"]
        assert other.request("test-enable-create")["enabled"]
        assert scenario.request("test-enable-gx")["enabled"]
        assert other.request("test-enable-gx")["enabled"]
        shared = "qmq-shared-session"
        assert paired_device.call("session", "create", "--host", scenario.host, "--session-id", shared)["lifecycle_receipt"]["session_id"] == shared
        assert paired_device.call("session", "create", "--host", other.host, "--session-id", shared)["lifecycle_receipt"]["session_id"] == shared

        assert paired_device.call("status")["default_host"] == other.host

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
        first_model = paired_device.call("session", "model", "get", "--host", scenario.host, "--session", shared)
        second_model = paired_device.call("session", "model", "get", "--host", other.host, "--session", shared)
        assert first_model["next"]["model"] == first_selection["model"]
        assert second_model["next"]["model"] == second_selection["model"]
        assert first_model["next"]["model"] != second_model["next"]["model"]
        default_model = paired_device.call("session", "model", "get", "--session", shared)
        assert default_model["next"]["model"] == second_model["next"]["model"]
        # Queue and question reads with an explicit Host answer for that Host only.
        for host in (scenario.host, other.host):
            queue = paired_device.call("session", "queue", "list", "--host", host, "--session", shared)
            assert queue["session_id"] == shared
            assert isinstance(queue["items"], list)
            assert isinstance(paired_device.call("session", "question", "list", "--host", host, "--session", shared)["batches"], list)
    finally:
        other.close()
