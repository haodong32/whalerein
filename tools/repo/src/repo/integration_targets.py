"""Public Linux test leaves, their collection paths, and their build inputs."""

from dataclasses import dataclass
from pathlib import Path

from .errors import UsageError


@dataclass(frozen=True)
class Leaf:
    modules: tuple[str, ...]
    artifacts: tuple[str, ...]

    @property
    def paths(self):
        return tuple(f"tests/integration/cases/test_{name}.py" for name in self.modules)


SCENARIO = ("access", "broker-plugin", "node-dependencies")
CLI = (*SCENARIO, "cli")
LEAVES = {
    "protocol-vectors": Leaf(
        ("acc02_host_auth_vectors", "observation_artifact", "proto_compat", "protocol"),
        ("node-dependencies", "noise-peer", "proto-peer"),
    ),
    "protocol-http": Leaf(("acc_http_contract",), CLI),
    "storage": Leaf(("storage", "stream_cache"), (*CLI, "storage-tests")),
    "transport": Leaf(
        ("transport", "transport_presence", "transport_tls"),
        (*CLI, "mqtt-driver", "broker-tests", "server-tests"),
    ),
    "chain": Leaf(
        (
            "acc01_presence_unavailable",
            "acc08_invitation_budget",
            "acc10_revocation_idempotency",
            "approval_delegate",
            "approval_lifecycle",
            "approval_multihost",
            "approval_restart",
            "approval_revocation",
            "chain",
            "chain_forwarding_recovery",
            "cli_contract",
            "creation_acceptance_extra",
            "cutover_host_state",
            "gx_faults",
            "journeys",
            "pair_invitation_contract",
            "pair_invitation_single_use",
            "pair_prompt_cancel",
            "qmq_model",
            "qmq_queue",
            "qmq_questions",
            "queue_capability",
            "request_stale_recovery",
            "sess_create",
            "sess_discovery",
            "stream_concurrency",
            "stream_exit",
            "stream_fields",
            "stream_once",
            "stream_reconnect",
            "wait_deadline",
        ),
        (*CLI, "delivery-driver", "wait-driver"),
    ),
    "native-dsh": Leaf(
        (
            "client_cutover",
            "global_pair_commit_race",
            "global_pending_pairing_revoke",
            "gx03_stress",
            "gx_native_creation_variants",
            "gx_native_delivery_recovery",
            "gx_native_fork_recovery",
            "gx_native_interrupt_recovery",
            "gx_native_model_recovery",
            "gx_native_question_recovery",
            "gx_native_queue_recovery",
            "gx_native_stale_wait",
            "gx_rollout",
            "gx_workflows",
            "journeys_native",
            "late_pairing_ack",
            "multihost_committed_unavailable",
            "multihost_finish",
            "multihost_outcome_cleanup",
            "multihost_outcome_unconfirmed",
            "multihost_routing",
            "native_creation_multihost_refresh",
            "native_delivery_production_construction",
            "native_delivery_unconfirmed_receipt",
            "native_dsh",
            "native_lifecycle",
            "native_presence",
            "native_presence_restart",
            "native_revoked_identity",
            "native_settings",
            "pair_host_revoke_race",
            "pair_offline_host_revocation",
            "pair_precommit_interruption",
            "pair_revoke_race",
            "primary_host_repair",
            "qmq_multihost",
            "qmq_native",
            "two_node_operation",
        ),
        CLI,
    ),
}
TARGETS = tuple(LEAVES)


def select_leaves(target):
    if target in (None, "integration"):
        return TARGETS
    if target == "protocol":
        return ("protocol-vectors", "protocol-http")
    if target in LEAVES:
        return (target,)
    raise UsageError(f"Unknown Linux integration target: {target}")


def collection_paths(target):
    return tuple(sorted(path for name in select_leaves(target) for path in LEAVES[name].paths))


def selected_artifacts(target):
    return tuple(
        dict.fromkeys(
            artifact for name in select_leaves(target) for artifact in LEAVES[name].artifacts
        )
    )


def validate_inventory(root: Path):
    defined = collection_paths("integration")
    actual = {
        path.relative_to(root).as_posix() for path in root.glob("tests/integration/cases/test_*.py")
    }
    if len(defined) != len(set(defined)) or set(defined) != actual:
        raise UsageError(
            "Linux integration leaves must cover every test module exactly once "
            f"(unregistered: {sorted(actual.difference(defined))}, "
            f"missing: {sorted(set(defined).difference(actual))})"
        )
