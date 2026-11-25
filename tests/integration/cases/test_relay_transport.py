"""AC-01/02/05/15 relay wire evidence on the real broker.

Each case runs one Node probe that uses the real Host ``RelayTransport`` (or
``MqttAccess``) and a real Mosquitto connection. The probe prints only sizes,
representation names and stable codes; Python owns the case identity and the
assertion. The in-process directory stub is not evidence for Access storage: the
real directory appears in ``test_relay_authorization.py`` and
``test_relay_storage.py``.
"""

import json
import os
import subprocess

import pytest
from environment import BROKER_PASSWORD, ROOT

pytestmark = pytest.mark.transport

PROBE = ROOT / "tests/integration/drivers/transport/relay-epoch-probe.mjs"


def run_probe(case, broker):
    env = {
        **os.environ,
        "WHALEREIN_TEST_MQTT_URL": broker,
        "WHALEREIN_TEST_ACCESS_PASSWORD": BROKER_PASSWORD,
        "WHALEREIN_TEST_HOST_PASSWORD": "host-pass",
    }
    result = subprocess.run(
        ["node", str(PROBE), case],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, f"relay probe {case} failed: {result.stderr.strip()}"
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    assert lines, f"relay probe {case} emitted no facts"
    return json.loads(lines[-1])


@pytest.mark.parametrize(
    "case,subcase",
    [
        pytest.param(
            "receiver-budget",
            "AC-01.receiver-budget",
            id="AC-01.receiver-budget",
            marks=pytest.mark.description(
                "When a receiver advertises a smaller packet budget, the same complete message stays inline below that budget and becomes one packet-sized reference above it."
            ),
        ),
        pytest.param(
            "second-hop-reference",
            "AC-01.second-hop-reference",
            id="AC-01.second-hop-reference",
            marks=pytest.mark.description(
                "When the first hop carries one complete inline envelope and the next hop advertises a smaller budget, the second hop publishes a reference only after the exact object is readable and the message identity survives."
            ),
        ),
        pytest.param(
            "receiver-budget",
            "AC-15.asymmetric-hop-budgets",
            id="AC-15.asymmetric-hop-budgets",
            marks=pytest.mark.description(
                "When two hops advertise different packet budgets, each hop uses the smallest authenticated value and a smaller second hop forces a reference without raising the first hop."
            ),
        ),
        pytest.param(
            "reference-limit",
            "AC-01.reference-limit",
            id="AC-01.reference-limit",
            marks=pytest.mark.description(
                "When even a reference packet would exceed the hop limit, publication fails with the stable payload_too_large error before any PUBLISH and without reserving an object."
            ),
        ),
        pytest.param(
            "oversize",
            "AC-05.oversize",
            id="AC-05.oversize",
            marks=pytest.mark.description(
                "When a complete message exceeds the envelope bound, the sender fails explicitly with payload_too_large, publishes nothing and never falls back to fragments."
            ),
        ),
        pytest.param(
            "legacy",
            "AC-05.legacy",
            id="AC-05.legacy",
            marks=pytest.mark.description(
                "When a relay envelope names epoch 0, 2 or the retired epochs 3 and 4, or mixes inline and reference representations, it is rejected as protocol_version_incompatible or invalid_argument and never reaches Host business handling."
            ),
        ),
        pytest.param(
            "matched-no-fragments",
            "AC-05.matched",
            id="AC-05.matched",
            marks=pytest.mark.description(
                "When a matching epoch-5 release publishes six small results at the negotiated budget, every body arrives intact, the publisher does not reconnect, and no fragment, receipt or acknowledgement packet appears."
            ),
        ),
        pytest.param(
            "matched-no-fragments",
            "AC-05.no-fragments",
            id="AC-05.no-fragments",
            marks=pytest.mark.description(
                "When a matching epoch-5 release publishes complete packets, the capture contains no WRMQTT2 prefix, no receipt topic and no device acknowledgement topic."
            ),
        ),
        pytest.param(
            "bulk-fairness",
            "AC-05.bulk-fairness",
            id="AC-05.bulk-fairness",
            marks=pytest.mark.description(
                "When a bulk reference publication waits for its object to become readable, a small inline flow on another topic is still published within the same connection."
            ),
        ),
        pytest.param(
            "live-class",
            "AC-15.live-topic-class",
            id="AC-15.live-topic-class",
            marks=pytest.mark.description(
                "When a Host publishes a preview, it arrives only on the QoS 0 live topic, the control event topic carries no preview, and the same complete envelope on the control path stays QoS 1."
            ),
        ),
        pytest.param(
            "live-inline-and-drop",
            "AC-15.live-inline-and-drop",
            id="AC-15.live-inline-and-drop",
            marks=pytest.mark.description(
                "When live delivery is disconnected, above the packet budget or would need an HTTPS reference, the preview is dropped without a retry or reference and the control event is still delivered at QoS 1."
            ),
        ),
        pytest.param(
            "fetch-verification",
            "AC-02.corrupt",
            id="AC-02.corrupt",
            marks=pytest.mark.description(
                "When a fetched object's bytes do not match the reference digest, the real Host reader rejects it with payload_digest_mismatch before business handling."
            ),
        ),
        pytest.param(
            "fetch-verification",
            "AC-02.truncated",
            id="AC-02.truncated",
            marks=pytest.mark.description(
                "When a fetched object is shorter than the declared reference length, the real Host reader rejects it as invalid_argument instead of returning a partial body."
            ),
        ),
    ],
)
def test_relay_transport(case, subcase, broker):
    """Passes the named epoch-5 relay subcase on the real broker."""
    facts = run_probe(case, broker)
    if subcase == "AC-01.receiver-budget":
        assert facts["inlineBytes"] <= facts["limit"]
        assert facts["referenceBytes"] <= facts["limit"]
    elif subcase == "AC-01.second-hop-reference":
        assert facts["first"] == "inline" and facts["second"] == "reference"
    elif subcase == "AC-15.asymmetric-hop-budgets":
        assert facts["inlineBytes"] <= facts["limit"]
        assert facts["referenceBytes"] <= facts["limit"]
    elif subcase == "AC-01.reference-limit":
        assert facts["code"] == "payload_too_large" and facts["packets"] == 0
    elif subcase == "AC-05.oversize":
        assert facts["code"] == "payload_too_large" and facts["packets"] == 0
    elif subcase == "AC-05.legacy":
        assert facts["codes"] == {
            "0": "protocol_version_incompatible",
            "2": "protocol_version_incompatible",
            "3": "protocol_version_incompatible",
            "4": "protocol_version_incompatible",
            "mixed": "invalid_argument",
        }
        assert facts["legacyDelivered"] == 0 and facts["validDelivered"] == 1
    elif subcase == "AC-05.matched":
        assert facts["packets"] == 6 and facts["reconnects"] == 0
    elif subcase == "AC-05.no-fragments":
        assert facts["legacy"] == 0
    elif subcase == "AC-05.bulk-fairness":
        assert facts["small"] == "inline" and facts["bulk"] == "reference"
    elif subcase == "AC-02.corrupt":
        assert facts["codes"]["inline"] == "payload_digest_mismatch"
    elif subcase == "AC-02.truncated":
        assert facts["codes"]["truncated"] in {"invalid_argument", "unavailable"}
    elif subcase == "AC-15.live-topic-class":
        assert facts["liveQos"] == 0 and facts["controlQos"] == 1
        assert facts["livePackets"] == 1 and facts["eventPackets"] == 1
    elif subcase == "AC-15.live-inline-and-drop":
        assert facts["dropped"] is False and facts["oversized"] is False and facts["referenced"] is False
        assert facts["control"] == "inline" and facts["controlQos"] == 1
    else:  # pragma: no cover - the parametrization is exhaustive
        raise AssertionError(f"unbound relay subcase {subcase}")


# relay/AC-05.quota is intentionally not a case: the locked Mosquitto 2.1.4
# configuration exposes no measurable inbound Receive Maximum and no product
# seam reports unacknowledged PUBLISH count. It stays BLOCKED in the acceptance
# record instead of becoming a skipped test that could be mistaken for a pass.
