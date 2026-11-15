import io
import json

import pytest

from repo.output import SCHEMA, Output, redact


def test_json_mode_writes_result_envelope_only_to_stdout():
    stdout, stderr = io.StringIO(), io.StringIO()
    out = Output(json_mode=True, verbose=True, stdout=stdout, stderr=stderr)
    out.info("hello")
    out.error("bad")
    out.result("doctor", ok=True, data={"a": 1})
    assert stderr.getvalue().startswith("hello")
    assert "bad" in stderr.getvalue()
    payload = json.loads(stdout.getvalue())
    assert payload == {"schema": SCHEMA, "command": "doctor", "ok": True, "data": {"a": 1}}


def test_failed_json_result_includes_stable_error_envelope():
    stdout = io.StringIO()
    out = Output(json_mode=True, stdout=stdout, stderr=io.StringIO())
    out.result("test", ok=False)
    payload = json.loads(stdout.getvalue())
    assert payload["ok"] is False
    assert payload["error"]["code"] == "error"


def test_human_mode_does_not_emit_json_result_envelope():
    stdout = io.StringIO()
    out = Output(stdout=stdout, stderr=io.StringIO())
    out.result("test", ok=True, data={"a": 1})
    assert stdout.getvalue() == ""


def test_quiet_hides_messages_and_verbose_shows_detail():
    stdout, stderr = io.StringIO(), io.StringIO()
    out = Output(quiet=True, stdout=stdout, stderr=stderr)
    out.info("hidden")
    out.detail("hidden detail")
    assert stdout.getvalue() == ""
    out2 = Output(verbose=True, stdout=io.StringIO(), stderr=io.StringIO())
    out2.detail("shown")
    assert "shown" in out2.stdout.getvalue()


def test_redaction_hides_credentials_and_preserves_plain_text():
    assert "secret" not in redact("x?token=secret").lower()
    assert "abc" not in redact("Authorization: Bearer abc.def")
    assert redact("plain text") == "plain text"


@pytest.mark.parametrize(
    "sensitive",
    [
        'token="synthetic credential with spaces"',
        "'apiKey': 'synthetic credential with spaces'",
        'private_key: "synthetic\\" nested quote"',
        'deviceToken = "synthetic\\" nested quote"',
        "Authorization: Bearer synthetic.canary",
        "Authorization: Basic c3ludGhldGlj",
        "-----BEGIN PRIVATE KEY-----\nsynthetic key material\n-----END PRIVATE KEY-----",
        "-----BEGIN PRIVATE KEY-----\nsynthetic incomplete key material",
        'tool_parameters: {"outer": ["synthetic payload", {"nested": "synthetic tail"}]}',
        'message_content: "synthetic body with spaces"',
        "message_content: synthetic plain body\nsynthetic continuation",
        "whalerein://pair?v=1&host=synthetic-host&token=synthetic-token",
        "postgresql://operator:synthetic-dsn-credential@localhost/database",
    ],
    ids=[
        "quoted-token",
        "camel-case-api-key",
        "escaped-quote-private-key",
        "escaped-quote-device-token",
        "bearer-header",
        "basic-header",
        "pem-key",
        "unterminated-pem-key",
        "nested-tool-parameters",
        "quoted-message-content",
        "unquoted-multiline-message-content",
        "whole-pairing-invitation",
        "url-userinfo-credentials",
    ],
)
def test_diagnostics_redact_complete_sensitive_values(sensitive):
    from repo.diagnostics import sanitize

    result = sanitize("failure: " + sensitive)
    assert "synthetic" not in result, "sensitive diagnostic value was only partly redacted"
    assert result.startswith("failure:")
    assert "redacted" in result


@pytest.mark.parametrize("channel", ["error", "warn", "detail"])
def test_output_diagnostic_channels_apply_the_same_redaction_and_limit(channel):
    from repo.diagnostics import LIMIT

    stdout, stderr = io.StringIO(), io.StringIO()
    out = Output(verbose=True, stdout=stdout, stderr=stderr)
    getattr(out, channel)('password: "synthetic diagnostic canary" ' + "x" * (LIMIT + 100))
    rendered = stdout.getvalue() + stderr.getvalue()
    assert "synthetic" not in rendered
    assert "[diagnostic truncated]" in rendered
    assert len(rendered) < LIMIT + 100


@pytest.mark.parametrize(
    "code",
    ["token=synthetic-code", "arbitrary_synthetic_metadata", ["synthetic"]],
    ids=["credential-shaped-code", "unregistered-code", "non-string-code"],
)
def test_failed_json_result_has_stable_code_and_sanitizes_nested_error_data(code):
    stdout = io.StringIO()
    out = Output(json_mode=True, stdout=stdout, stderr=io.StringIO())
    out.result(
        "test",
        ok=False,
        data={"token": "synthetic-value", "steps": [{"error": 'api_key: "synthetic-key"'}]},
        error={
            "code": code,
            "message": 'operation failed password="synthetic-message"',
            "extra": "synthetic-extra",
        },
    )
    rendered = stdout.getvalue()
    payload = json.loads(rendered)
    assert payload["error"]["code"] == "error"
    assert payload["data"]["token"] == "<redacted>"
    assert len(payload["data"]["steps"]) == 1
    assert "synthetic" not in rendered


def test_successful_explicit_result_and_case_identity_are_preserved():
    identity = "tests/test_parameters.py::test_selection[token=synthetic-example]"
    stdout = io.StringIO()
    out = Output(json_mode=True, stdout=stdout, stderr=io.StringIO())
    out.result("list", ok=True, data={"id": identity})
    assert json.loads(stdout.getvalue())["data"]["id"] == identity
    human = io.StringIO()
    Output(stdout=human).info(identity)
    assert human.getvalue().strip() == identity


def test_oserror_diagnostics_expose_errno_without_private_payloads():
    import errno

    stderr = io.StringIO()
    out = Output(verbose=True, stdout=io.StringIO(), stderr=stderr)
    out.failure(
        "test", OSError(errno.ENOSPC, "synthetic-private-message", "synthetic-private-path")
    )
    assert "OSError (errno=28)" in stderr.getvalue()
    assert "synthetic-private" not in stderr.getvalue()
