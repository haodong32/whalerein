"""AC-05/AC-12 hard-cut evidence against a real Access process.

The transport probe covers raw packet behavior. These cases prove the HTTP
admission boundary in epoch 5: only the durable command lane remains on HTTP,
the retired query/handshake kinds are rejected without creating a row, and a
retired relay epoch is named explicitly instead of falling back.
"""

import base64
import json
import urllib.parse
import time
import uuid

import pytest
import urllib.request

from environment import INTERNAL_TOKEN, relay_http, wait
from journey_helpers import host_online, sql, wait_online

pytestmark = pytest.mark.transport


def query_body(scenario, device, **overrides):
    payload = {
        "requestId": uuid.uuid4().hex,
        "hostId": scenario.host,
        "deviceId": device.identity["device_id"],
        "kind": "REQUEST_KIND_QUERY",
        "expiresAtUnixMs": str(int((time.time() + 60) * 1000)),
        "protocolVersion": 1,
        "channelId": "channel-cutover",
        "relayVersion": 5,
        "body": base64.b64encode(b"opaque ciphertext").decode(),
    }
    payload.update(overrides)
    return payload


def command_body(scenario, device, **overrides):
    payload = {
        "requestId": uuid.uuid4().hex,
        "hostId": scenario.host,
        "deviceId": device.identity["device_id"],
        "commandId": uuid.uuid4().hex,
        "payloadDigest": base64.b64encode(b"d" * 32).decode(),
        "kind": "REQUEST_KIND_COMMAND",
        "expiresAtUnixMs": str(int((time.time() + 60) * 1000)),
        "protocolVersion": 1,
        "channelId": "channel-cutover",
        "relayVersion": 5,
        "body": base64.b64encode(b"opaque ciphertext").decode(),
    }
    payload.update(overrides)
    return payload


def request_row(scenario, request_id):
    """Read only the admission identity of one durable request row, if any.

    ``sql`` returns the psql ``SET`` command tag when the statement produces no
    rows, so the probe always returns exactly one row (empty when absent).
    """
    raw = sql(
        scenario.schema,
        "SELECT coalesce((SELECT kind::text || '|' || status || '|' || relay_version::text "
        f"FROM requests WHERE request_id = '{request_id}'), 'absent')",
    )
    if not raw or raw == "absent":
        return None
    kind, status, relay_version = raw.split("|")
    return {"kind": int(kind), "status": status, "relay_version": int(relay_version)}


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
                "When a command names relay epoch 0, Access rejects it with protocol_version_incompatible and never admits or forwards it."
            ),
        ),
        pytest.param(
            2,
            id="AC-05.legacy-v2",
            marks=pytest.mark.description(
                "When a command names relay epoch 2, Access rejects it with protocol_version_incompatible and never admits or forwards it."
            ),
        ),
        pytest.param(
            3,
            id="AC-05.legacy-v3",
            marks=pytest.mark.description(
                "When a command names the retired relay epoch 3, Access rejects it with protocol_version_incompatible and never admits or forwards it."
            ),
        ),
        pytest.param(
            4,
            id="AC-05.legacy-v4",
            marks=pytest.mark.description(
                "When a command names the retired relay epoch 4, Access rejects it with protocol_version_incompatible instead of falling back; only epoch 5 is accepted."
            ),
        ),
    ],
)
def test_legacy_epoch_is_rejected(scenario, paired_device, epoch):
    """A durable request that does not name epoch 5 is rejected with protocol_version_incompatible and is not admitted."""
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, body = post_request(
            scenario,
            paired_device,
            bearer,
            command_body(scenario, paired_device, relayVersion=epoch),
        )
    assert status == 400
    payload = json.loads(body)
    assert payload["code"] == "protocol_version_incompatible"
    # The same command shape at epoch 5 is admitted, so the failure is the
    # cutover, not a broken fixture. There is no fallback for epoch 4.
    with scenario.fixture_bearer(paired_device) as bearer:
        accepted, _, accepted_body = post_request(
            scenario, paired_device, bearer, command_body(scenario, paired_device)
        )
    assert accepted == 200, accepted_body


def test_retired_http_query_and_handshake_are_rejected(scenario, paired_device):
    """Epoch-5 QUERY and HANDSHAKE requests over HTTP fail explicitly and create no durable row."""
    query = query_body(scenario, paired_device)
    handshake = query_body(
        scenario,
        paired_device,
        kind="REQUEST_KIND_HANDSHAKE",
        body=base64.b64encode(b"noise handshake").decode(),
    )
    with scenario.fixture_bearer(paired_device) as bearer:
        for payload in (query, handshake):
            status, _, body = post_request(scenario, paired_device, bearer, payload)
            assert status == 400, body
            assert json.loads(body)["code"] == "protocol_version_incompatible"
    # Neither retired kind left a durable request row; the query lane is the
    # only admission surface for them.
    for payload in (query, handshake):
        row = request_row(scenario, payload["requestId"])
        assert row is None, f"retired HTTP {payload['kind']} created a durable row"


def test_command_submission_stays_on_http(scenario, paired_device):
    """An epoch-5 COMMAND is admitted on the durable HTTPS route and creates the request row."""
    payload = command_body(scenario, paired_device)
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, body = post_request(scenario, paired_device, bearer, payload)
    assert status == 200, body
    receipt = json.loads(body)
    assert receipt.get("requestId") == payload["requestId"], receipt
    row = request_row(scenario, payload["requestId"])
    assert row is not None and row["kind"] == 3


def test_mixed_representation_is_rejected(scenario, paired_device):
    """A command carrying both bodies, or a reference with a mismatched digest, is invalid_argument."""
    reference = {
        "objectId": "f" * 32,
        "ownerNodeId": "node-integration",
        "ciphertextBytes": "16",
        "sha256": base64.b64encode(b"d" * 32).decode(),
        "expiresAtUnixMs": str(int((time.time() + 60) * 1000)),
        "purpose": "PAYLOAD_PURPOSE_REQUEST",
    }
    with scenario.fixture_bearer(paired_device) as bearer:
        both = command_body(
            scenario,
            paired_device,
            bodyReference=reference,
        )
        status, _, body = post_request(scenario, paired_device, bearer, both)
        assert status == 400 and json.loads(body)["code"] == "invalid_argument"

        mismatched = command_body(
            scenario,
            paired_device,
            body=None,
            bodyReference=reference,
            payloadDigest=base64.b64encode(b"x" * 32).decode(),
        )
        status, _, body = post_request(scenario, paired_device, bearer, mismatched)
        assert status == 400 and json.loads(body)["code"] in {"invalid_argument", "payload_digest_mismatch"}

        # The retired bootstrap kind is rejected before representation rules.
        bootstrap = query_body(
            scenario,
            paired_device,
            kind="REQUEST_KIND_HANDSHAKE",
            body=None,
            bodyReference=reference,
            payloadDigest=base64.b64encode(b"d" * 32).decode(),
        )
        status, _, body = post_request(scenario, paired_device, bearer, bootstrap)
        assert status == 400 and json.loads(body)["code"] == "protocol_version_incompatible"


@pytest.mark.description(
    "During a coordinated cohort quiesce no dual path admits queries, and matching artifacts resume the retained grant, pinned identity and session history without re-pairing while a retired enrollment stays retired."
)
def test_ac08_cohort_quiesce_and_release(scenario, paired_device):
    session = scenario.dsh.create_session()
    paired_device.sessions(scenario.host)
    assert paired_device.hosts()[0]["online"] is True

    # Capture readiness and the Broker policy topology before quiescing.
    with urllib.request.urlopen("http://127.0.0.1:18080/readyz", timeout=5) as ready:
        assert ready.status == 200
        ready_body = ready.read().decode()
    host_token = scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"]
    form = urllib.parse.urlencode({
        "clientid": "whalerein-host-" + scenario.host,
        "username": "host:" + scenario.host,
        "password": host_token,
    }).encode()
    policy_request = urllib.request.Request(
        "http://127.0.0.1:18081/internal/broker/connect",
        data=form,
        headers={"X-WhaleRein-Internal-Token": INTERNAL_TOKEN},
    )
    with urllib.request.urlopen(policy_request, timeout=5) as response:
        policy = response.read().decode()
    assert policy.startswith("whalerein/1"), policy[:40]
    for topic in ("/request", "/result", "/event", "/presence", "/d/+/live"):
        assert f"whalerein/v1/h/{scenario.host}{topic}" in policy, topic
    assert f"whalerein/v1/h/{scenario.host}/live" not in policy, "the retired Host live topic is still granted"
    assert len(ready_body) >= 0

    # Quiesce: the matching Host stops admitting work before the cutover.
    scenario.plugin.close()
    wait(lambda: not host_online(paired_device, scenario.host), timeout=40)
    code, values = paired_device.invoke(
        "session", "queue", "list", "--session", session, success=False
    )
    assert code != 0, "a query during quiesce must fail closed"
    assert values[-1]["error"]["code"] in {
        "host_offline", "unavailable", "not_found", "timeout", "forbidden",
    }

    # The retired HTTP admission route is still closed while admissions are
    # quiesced: no dual path can carry the query.
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, body = post_request(
            scenario,
            paired_device,
            bearer,
            query_body(scenario, paired_device, requestId="quiesced-" + uuid.uuid4().hex),
        )
    assert status != 200, body
    assert b"protocol_version_incompatible" in body

    # Matching artifacts restart: the retained grant, pinned identity and
    # source history resume without another pairing.
    scenario.start_plugin()
    wait_online(paired_device, scenario.host)
    assert paired_device.hosts()[0]["online"] is True
    assert [entry["session_id"] for entry in paired_device.sessions(scenario.host)] == [session]
    paired_device.history(session)
    paired_device.call("session", "queue", "list", "--session", session)

    # A retired enrollment stays retired after the cutover.
    paired_device.revoke()
    code, values = paired_device.invoke(
        "session", "queue", "list", "--session", session, success=False
    )
    assert code == 4, values[-1]
    assert values[-1]["error"]["code"] in {"revoked", "unauthorized", "not_paired"}
