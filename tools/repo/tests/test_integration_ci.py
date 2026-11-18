"""Scheduled reuse must never turn incomplete integration into a passed baseline."""

import json
from types import SimpleNamespace

import pytest

from repo import integration_ci as ci


@pytest.fixture
def report(tmp_path):
    document = {
        "configuration": {"suite": "integration"},
        "completeness": "complete",
        "scopes": [
            {
                "exit_code": 0,
                "completeness": "complete",
                "tests": [{"id": "case", "status": "passed"}],
            }
        ],
        "verification": {
            "purpose": "ci",
            "suite": "integration",
            "exit_code": 0,
            "cleanup": {"status": "complete", "errors": []},
            "sources": {
                "public": {"revision": "a" * 40, "mode": "commit", "fingerprint": "b" * 64}
            },
            "collected_cases": ["case"],
            "artifacts": {"products": {"source": "b" * 64}},
            "identities": {"test-container": {"expected": "image", "actual": "image"}},
        },
    }
    path = tmp_path / "report.json"
    path.write_text(json.dumps(document))
    return path, document


def test_fingerprint_ignores_only_documentation(monkeypatch, tmp_path):
    def digest(records):
        monkeypatch.setattr(ci.subprocess, "run", lambda *a, **kw: SimpleNamespace(stdout=records))
        return ci.fingerprint(tmp_path, "HEAD")

    base = b"100644 blob abc\tcore/src/lib.rs\0"
    assert digest(base) == digest(base + b"100644 blob def\tdocs/test.txt\0")
    assert digest(base) == digest(base + b"100644 blob def\tREADME.md\0")
    assert digest(base) != digest(base + b"100644 blob def\tCargo.lock\0")
    assert digest(base) != digest(base + b"100644 blob def\tnew-input\0")


def test_success_and_invalid_receipts(report, tmp_path):
    path, _ = report
    receipt = tmp_path / "passed/receipt.json"
    revision = "a" * 40
    ci.record_success(path, receipt, revision, "key", "b" * 64)
    assert ci.cached_revision(receipt, "key") == revision
    assert ci.cached_revision(receipt, "changed") == ""
    for value in [
        "broken",
        "{}",
        '{"suite": "unit"}',
        '{"suite":"integration","fingerprint":"key","revision":null}',
    ]:
        receipt.write_text(value)
        assert ci.cached_revision(receipt, "key") == ""
    receipt.unlink()
    assert ci.cached_revision(receipt, "key") == ""


@pytest.mark.parametrize("change", ["suite", "group", "only", "list", "incomplete", "empty"])
def test_partial_reports_never_establish_baseline(report, tmp_path, change):
    path, doc = report
    if change == "suite":
        doc["configuration"]["suite"] = "unit"
    elif change in ("group", "only"):
        doc["configuration"][change] = "partial"
    elif change == "list":
        doc["configuration"]["list_only"] = True
    elif change == "incomplete":
        doc["completeness"] = "incomplete"
    else:
        doc["scopes"] = []
    path.write_text(json.dumps(doc))
    with pytest.raises(ValueError):
        ci.record_success(path, tmp_path / "receipt", "a" * 40, "key", "b" * 64)
    assert not (tmp_path / "receipt").exists()


@pytest.mark.parametrize("change", ["exit", "errors", "incomplete", "empty", "skipped", "failed"])
def test_failure_and_cleanup_errors_never_establish_baseline(report, tmp_path, change):
    path, doc = report
    scope = doc["scopes"][0]
    if change == "exit":
        scope["exit_code"] = 130
    elif change == "errors":
        scope["errors"] = ["cleanup failed"]
    elif change == "incomplete":
        scope["completeness"] = "incomplete"
    elif change == "empty":
        scope["tests"] = []
    else:
        scope["tests"][0]["status"] = change
    path.write_text(json.dumps(doc))
    with pytest.raises(ValueError):
        ci.record_success(path, tmp_path / "receipt", "a" * 40, "key", "b" * 64)


def test_command_operations(monkeypatch, tmp_path, capsys):
    monkeypatch.setattr(ci, "fingerprint", lambda *a: "key")
    monkeypatch.setattr("sys.argv", ["integration_ci", "key", "--revision", "HEAD"])
    ci.main()
    assert capsys.readouterr().out.strip() == "key"
    monkeypatch.setattr(
        "sys.argv",
        ["integration_ci", "cached", "--revision", "HEAD", "--output", str(tmp_path / "missing")],
    )
    ci.main()
    assert capsys.readouterr().out.strip() == ""
    calls = []
    monkeypatch.setattr(ci, "record_success", lambda *args: calls.append(args))
    monkeypatch.setattr(
        ci,
        "capture_sources",
        lambda *args: {
            "public": SimpleNamespace(revision="commit", fingerprint="digest"),
        },
    )
    monkeypatch.setattr(
        "sys.argv",
        [
            "integration_ci",
            "record",
            "--revision",
            "HEAD",
            "--output",
            str(tmp_path / "receipt"),
            "--report",
            str(tmp_path / "report"),
        ],
    )
    ci.main()
    assert calls[0][-3:] == ("commit", "key", "digest")


@pytest.mark.parametrize(
    "change", ["source", "artifact", "cleanup", "runtime", "inventory", "local"]
)
def test_cannot_reuse_receipt_with_wrong_runtime_or_inputs(report, tmp_path, change):
    path, document = report
    facts = document["verification"]
    if change == "source":
        facts["sources"]["public"]["revision"] = "c" * 40
    elif change == "artifact":
        facts["artifacts"]["products"]["source"] = "c" * 64
    elif change == "cleanup":
        facts["cleanup"]["status"] = "failed"
    elif change == "runtime":
        facts["identities"]["test-container"]["actual"] = "stale"
    elif change == "inventory":
        facts["collected_cases"].append("missing")
    else:
        facts["purpose"] = "local"
    path.write_text(json.dumps(document))
    with pytest.raises(ValueError, match="expected inputs and scope"):
        ci.record_success(path, tmp_path / "receipt", "a" * 40, "key", "b" * 64)
