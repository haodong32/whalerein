"""ACC-05/07/12/13/14: live HTTP contract of the Access public listener."""

import json
import urllib.error
import urllib.request

import pytest

pytestmark = pytest.mark.protocol

BASE = "http://127.0.0.1:18080"


def _request(method, path, body=None, headers=None):
    """Return (status, headers, decoded-json-or-bytes) without raising on 4xx."""
    data = None
    if body is not None:
        data = body if isinstance(body, bytes) else body.encode()
    request = urllib.request.Request(
        BASE + path, data=data, headers=headers or {}, method=method
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            raw = response.read()
            return response.status, dict(response.headers), _decode(raw)
    except urllib.error.HTTPError as error:
        raw = error.read()
        return error.code, dict(error.headers), _decode(raw)


def _decode(raw):
    if not raw:
        return None
    try:
        return json.loads(raw)
    except ValueError:
        return raw


def _error(status, headers, body):
    assert 400 <= status < 500 or status == 501, (status, body)
    assert headers.get("X-Whalerein-Trace-Id"), headers
    assert isinstance(body, dict) and body.get("code"), body
    return body


@pytest.mark.description(
    "Additive JSON fields from a newer peer are accepted while an unknown enum name is rejected as protocol_version_incompatible without echoing it."
)
def test_additive_fields_are_accepted_and_unknown_enums_are_incompatible(scenario):
    status, _, invitation = _request(
        "POST",
        "/v1/pairing/invitations",
        json.dumps({"hostId": scenario.host, "futureField": {"added": True}}),
        {
            "Authorization": "Bearer " + scenario.admin,
            "Content-Type": "application/json",
        },
    )
    assert status == 200, invitation
    assert invitation["token"]

    status, headers, body = _request(
        "POST",
        "/v1/pairing/claims",
        json.dumps(
            {
                "token": invitation["token"],
                "claimId": "cross-version-enum",
                "phase": "PAIRING_PHASE_FROM_THE_FUTURE",
                "noiseMessage": "bXNn",
                "deviceId": "probe-device",
                "protocolVersion": 1,
            }
        ),
        {"Content-Type": "application/json"},
    )
    body = _error(status, headers, body)
    assert status == 400 and body["code"] == "protocol_version_incompatible", body
    assert "PAIRING_PHASE_FROM_THE_FUTURE" not in json.dumps(body)

    status, headers, body = _request(
        "POST",
        "/v1/pairing/claims",
        b'{"token":',
        {"Content-Type": "application/json"},
    )
    body = _error(status, headers, body)
    assert status == 400 and body["code"] == "invalid_argument", body
    assert "not valid JSON" in body["message"], body

    status, headers, body = _request(
        "POST",
        "/v1/pairing/claims",
        b"{}",
        {"Content-Type": "text/plain"},
    )
    body = _error(status, headers, body)
    assert status == 415 and body["code"] == "invalid_argument", body


@pytest.mark.description(
    "In standalone mode the external account routes answer an explicit 501 unsupported_capability while a mistyped path stays a plain 404."
)
def test_standalone_external_routes_report_a_capability_error(scenario):
    headers = {"Authorization": "Bearer " + scenario.admin}
    status, headers_in, body = _request(
        "GET", "/v1/enrollments/hosts/host-1", headers=headers
    )
    body = _error(status, headers_in, body)
    assert status == 501 and body["code"] == "unsupported_capability", body

    status, headers_in, body = _request("GET", "/v1/operations/op-1", headers=headers)
    body = _error(status, headers_in, body)
    assert status == 501 and body["code"] == "unsupported_capability", body

    status, headers_in, body = _request(
        "POST",
        "/v1/enrollments/hosts",
        json.dumps({"hostId": "host-1"}),
        {"Content-Type": "application/json"},
    )
    body = _error(status, headers_in, body)
    assert status == 501 and body["code"] == "unsupported_capability", body

    status, headers_in, body = _request("GET", "/v1/enrollment")
    body = _error(status, headers_in, body)
    assert status == 404 and body["code"] == "not_found", body


@pytest.mark.description(
    "The public listener advertises Allow on 405, answers OPTIONS with 204 and HEAD without a body, and never treats an unknown path as a method error."
)
def test_method_matrix_and_head_options(scenario):
    status, headers, body = _request("OPTIONS", "/v1/hosts")
    assert status == 204 and body is None, (status, body)
    assert headers.get("Allow") == "OPTIONS, GET, HEAD", headers

    status, headers, body = _request("POST", "/v1/hosts")
    body = _error(status, headers, body)
    assert status == 405 and headers.get("Allow") == "OPTIONS, GET, HEAD", headers

    status, headers, body = _request("HEAD", "/healthz")
    assert status == 200 and body is None, (status, body)

    status, headers, body = _request("OPTIONS", "/missing")
    body = _error(status, headers, body)
    assert status == 404 and body["code"] == "not_found", body


@pytest.mark.description(
    "A well-formed client trace id is echoed, while an unsafe trace id is regenerated and never reflected into a log-shaped response."
)
def test_trace_id_is_echoed_only_for_the_bounded_alphabet(scenario):
    status, headers, _ = _request(
        "GET", "/healthz", headers={"X-Whalerein-Trace-Id": "trace-live-1"}
    )
    assert status == 200
    assert headers.get("X-Whalerein-Trace-Id") == "trace-live-1", headers

    injected = 'safe {"level":"ERROR","msg":"forged"}'
    status, headers, body = _request(
        "GET", "/v1/hosts", headers={"X-Whalerein-Trace-Id": injected}
    )
    body = _error(status, headers, body)
    trace = headers.get("X-Whalerein-Trace-Id")
    assert status == 401
    assert trace != injected and len(trace) == 32, trace
    assert body["traceId"] == trace, body
    assert "forged" not in json.dumps(body) and "ERROR" not in json.dumps(body)


@pytest.mark.description(
    "Unsafe or overlong resource identifiers are rejected as invalid_argument at the HTTP boundary instead of reaching SQL and failing as a 500."
)
def test_unbounded_resource_ids_are_rejected_at_the_boundary(scenario, paired_device):
    token = "Bearer " + paired_device.transport_token()
    overlong = "a" * 129
    for method, path in [
        ("GET", "/v1/requests/" + overlong),
        ("GET", "/v1/requests/bad$id"),
        ("DELETE", "/v1/devices/" + overlong),
        ("POST", "/v1/hosts/" + overlong + "/requests"),
    ]:
        status, headers, body = _request(method, path, headers={"Authorization": token})
        body = _error(status, headers, body)
        assert status == 400 and body["code"] == "invalid_argument", (path, status, body)

    status, headers, body = _request("GET", "/v1/requests/missing-request", headers={"Authorization": token})
    assert status == 404, (status, body)
