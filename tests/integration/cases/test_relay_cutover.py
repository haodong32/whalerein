"""AC-05 hard-cut evidence against a real Access process.

The transport probe covers raw packet behavior. These cases prove the HTTP
admission boundary: an old relay epoch is named explicitly and a mixed
representation is rejected before a row is admitted or forwarded.
"""

import base64
import json
import time
import uuid

import pytest
from environment import relay_http

pytestmark = pytest.mark.transport


def request_body(scenario, device, **overrides):
    payload = {
        "requestId": uuid.uuid4().hex,
        "hostId": scenario.host,
        "deviceId": device.identity["device_id"],
        "kind": "REQUEST_KIND_QUERY",
        "expiresAtUnixMs": str(int((time.time() + 60) * 1000)),
        "protocolVersion": 1,
        "channelId": "channel-cutover",
        "relayVersion": 4,
        "body": base64.b64encode(b"opaque ciphertext").decode(),
    }
    payload.update(overrides)
    return payload


def post_request(scenario, device, token, payload):
    return relay_http(
        f"/v1/hosts/{scenario.host}/requests",
        "POST",
        json.dumps(payload).encode(),
        token,
        {"Content-Type": "application/json"},
    )


@pytest.mark.parametrize(
    "epoch",
    [
        pytest.param(
            0,
            id="AC-05.legacy-v0",
            marks=pytest.mark.description(
                "When a request names relay epoch 0, Access rejects it with protocol_version_incompatible and never admits or forwards it."
            ),
        ),
        pytest.param(
            2,
            id="AC-05.legacy-v2",
            marks=pytest.mark.description(
                "When a request names relay epoch 2, Access rejects it with protocol_version_incompatible and never admits or forwards it."
            ),
        ),
        pytest.param(
            3,
            id="AC-05.legacy-v3",
            marks=pytest.mark.description(
                "When a request names the retired relay epoch 3, Access rejects it with protocol_version_incompatible and never admits or forwards it; only epoch 4 is accepted."
            ),
        ),
    ],
)
def test_legacy_epoch_is_rejected(scenario, paired_device, epoch):
    """A relay envelope that does not name epoch 4 is rejected with protocol_version_incompatible and is not admitted."""
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, body = post_request(
            scenario,
            paired_device,
            bearer,
            request_body(scenario, paired_device, relayVersion=epoch),
        )
    assert status == 400
    payload = json.loads(body)
    assert payload["code"] == "protocol_version_incompatible"
    # The same payload at epoch 4 is admitted, so the failure is the cutover,
    # not a broken fixture. There is no fallback for the retired epoch 3.
    with scenario.fixture_bearer(paired_device) as bearer:
        accepted, _, _ = post_request(
            scenario, paired_device, bearer, request_body(scenario, paired_device)
        )
    assert accepted == 200


def test_mixed_representation_is_rejected(scenario, paired_device):
    """A request carrying both inline and referenced bodies, or a reference with a mismatched digest, is invalid_argument."""
    reference = {
        "objectId": "f" * 32,
        "ownerNodeId": "node-integration",
        "ciphertextBytes": "16",
        "sha256": base64.b64encode(b"d" * 32).decode(),
        "expiresAtUnixMs": str(int((time.time() + 60) * 1000)),
        "purpose": "PAYLOAD_PURPOSE_REQUEST",
    }
    with scenario.fixture_bearer(paired_device) as bearer:
        both = request_body(
            scenario, paired_device, bodyReference=reference, payloadDigest=base64.b64encode(b"d" * 32).decode()
        )
        status, _, body = post_request(scenario, paired_device, bearer, both)
        assert status == 400 and json.loads(body)["code"] == "invalid_argument"

        mismatched = request_body(
            scenario,
            paired_device,
            body=None,
            bodyReference=reference,
            payloadDigest=base64.b64encode(b"x" * 32).decode(),
        )
        status, _, body = post_request(scenario, paired_device, bearer, mismatched)
        assert status == 400 and json.loads(body)["code"] in {"invalid_argument", "payload_digest_mismatch"}

        bootstrap = request_body(
            scenario,
            paired_device,
            kind="REQUEST_KIND_HANDSHAKE",
            body=None,
            bodyReference=reference,
            payloadDigest=base64.b64encode(b"d" * 32).decode(),
        )
        status, _, body = post_request(scenario, paired_device, bearer, bootstrap)
        assert status == 400 and json.loads(body)["code"] == "invalid_argument"
