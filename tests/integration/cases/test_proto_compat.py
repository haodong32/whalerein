"""Protocol contract and version-compatibility integration cases.

These cases use only local files and tools (protoc, Python, Node); they do not
need PostgreSQL, a broker, or a device. They are still part of the ``protocol``
group so the coordinator runs them with the rest of the protocol suite.
"""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.protocol

ROOT = Path(__file__).resolve().parents[3]
DRIVER = ROOT / "tests/integration/drivers/protobuf/proto_compat.py"
SWIFT_CHECK = ROOT / "proto/v1/bindings/swift/check_swift_surface.py"
PROTO = ROOT / "proto/v1/whalerein.proto"
BASELINE = ROOT / "proto/v1/compat/baseline.json"
SURFACE = ROOT / "proto/v1/compat/surface.json"
VECTORS = ROOT / "proto/v1/compat/vectors.json"
ERROR_CODES = ROOT / "proto/v1/error_codes.json"
WIRE = ROOT / "proto/v1/WIRE.md"


def run(argv: list[str], *, timeout: int = 300) -> subprocess.CompletedProcess:
    return subprocess.run(
        argv, cwd=ROOT, capture_output=True, text=True, timeout=timeout, check=False
    )


@pytest.mark.description(
    "When the compiled schema is compared with the checked-in baseline and snapshot, a removed field, reused tag, changed type or changed oneof fails."
)
def test_schema_compatibility_baseline():
    result = run([sys.executable, str(DRIVER), "--check"])
    assert result.returncode == 0, result.stderr[-2000:]
    assert "protobuf compatibility passed" in result.stdout


@pytest.mark.description(
    "When the schema changes, --write cannot remove or renumber a field and --update-baseline refuses a breaking change without a rationale."
)
def test_write_cannot_bless_a_breaking_change(tmp_path):
    (tmp_path / "proto/v1/compat").mkdir(parents=True)
    (tmp_path / "tests/fixtures/proto").mkdir(parents=True)
    shutil.copy(PROTO, tmp_path / "proto/v1/whalerein.proto")
    shutil.copy(BASELINE, tmp_path / "proto/v1/compat/baseline.json")
    shutil.copy(ROOT / "tests/fixtures/proto/fields.json", tmp_path / "tests/fixtures/proto/fields.json")

    schema = (tmp_path / "proto/v1/whalerein.proto").read_text(encoding="utf-8")
    breaking = schema.replace("  bool running = 6;\n", "", 1)
    assert breaking != schema
    (tmp_path / "proto/v1/whalerein.proto").write_text(breaking, encoding="utf-8")

    write = run([sys.executable, str(DRIVER), "--root", str(tmp_path), "--write"])
    assert write.returncode != 0
    assert "removed field SessionSummary.running" in write.stderr
    assert "--update-baseline" in write.stderr

    unjustified = run([sys.executable, str(DRIVER), "--root", str(tmp_path), "--update-baseline"])
    assert unjustified.returncode != 0
    assert "refusing to bless a breaking schema change" in unjustified.stderr

    justified = run(
        [
            sys.executable,
            str(DRIVER),
            "--root",
            str(tmp_path),
            "--update-baseline",
            "--rationale",
            "protocol v2 removes running in favor of run_phase",
        ]
    )
    assert justified.returncode == 0, justified.stderr[-2000:]
    assert "removed field SessionSummary.running" in (
        tmp_path / "proto/v1/compat/CHANGELOG.md"
    ).read_text(encoding="utf-8")


@pytest.mark.description(
    "When an additive field is added, --write accepts it and refreshes the baseline so a later removal is still caught."
)
def test_write_accepts_an_additive_field(tmp_path):
    (tmp_path / "proto/v1/compat").mkdir(parents=True)
    (tmp_path / "tests/fixtures/proto").mkdir(parents=True)
    shutil.copy(PROTO, tmp_path / "proto/v1/whalerein.proto")
    shutil.copy(BASELINE, tmp_path / "proto/v1/compat/baseline.json")
    shutil.copy(ROOT / "tests/fixtures/proto/fields.json", tmp_path / "tests/fixtures/proto/fields.json")

    schema = (tmp_path / "proto/v1/whalerein.proto").read_text(encoding="utf-8")
    additive = schema.replace("message ListSessions {}\n", "message ListSessions { string future_label = 99; }\n", 1)
    assert additive != schema
    (tmp_path / "proto/v1/whalerein.proto").write_text(additive, encoding="utf-8")

    write = run([sys.executable, str(DRIVER), "--root", str(tmp_path), "--write"])
    assert write.returncode == 0, write.stderr[-2000:]
    baseline = json.loads((tmp_path / "proto/v1/compat/baseline.json").read_text())
    numbers = [field["number"] for field in baseline["messages"]["ListSessions"]["fields"]]
    assert 99 in numbers

    check = run([sys.executable, str(DRIVER), "--root", str(tmp_path), "--check"])
    assert check.returncode == 0, check.stderr[-2000:]


@pytest.mark.description(
    "When the wire compatibility vectors are loaded, they cover both reader directions, unknown enums, a missing oneof and exact 64-bit integers."
)
def test_compatibility_vectors_cover_both_directions():
    data = json.loads(VECTORS.read_text(encoding="utf-8"))
    assert data["schema"] == "whalerein/proto-compat-vectors/v1"
    entries = {entry["id"]: entry for entry in data["vectors"]}
    assert entries["json-unknown-field-is-ignored"]["kind"] == "json-decode"
    assert entries["json-old-reader-accepts-missing-new-fields"]["kind"] == "json-decode"
    assert entries["binary-unknown-tag-is-ignored"]["kind"] == "binary-decode"
    for identity in (
        "json-unknown-enum-name-is-explicit",
        "json-unknown-enum-number-needs-validation",
    ):
        assert entries[identity]["kind"].startswith("json-")
        assert entries[identity]["errorCode"] == "protocol_version_incompatible"
    assert entries["request-payload-missing-oneof"]["kind"] == "oneof-required"
    assert entries["request-payload-missing-oneof"]["errorCode"] == "invalid_argument"
    assert entries["int64-near-two-to-the-53"]["json"]["expiresAtUnixMs"] == "9007199254740993"
    assert entries["int64-near-two-to-the-53"]["expect"]["expiresAtUnixMs"] == "9007199254740993"
    languages = {language for entry in data["vectors"] for language in entry["languages"]}
    assert languages == {"rust", "go", "node"}


def _wire_error_table() -> dict[str, tuple[int, str]]:
    text = WIRE.read_text(encoding="utf-8")
    section = text.split("## CLI exit codes", 1)[1].split("\n## ", 1)[0]
    table: dict[str, tuple[int, str]] = {}
    for line in section.splitlines():
        if not line.startswith("| `"):
            continue
        columns = [column.strip() for column in line.split("|")]
        name = columns[1].strip("`")
        if name == "ErrorCode":
            continue
        table[name] = (int(columns[2]), columns[3].strip("`"))
    return table


@pytest.mark.description(
    "When the documented error table is compared with the golden table, every wire code keeps one exit code and one client action."
)
def test_error_code_golden_table():
    table = json.loads(ERROR_CODES.read_text(encoding="utf-8"))
    assert table["schema"] == "whalerein/error-codes/v1"
    documented = _wire_error_table()
    names = [entry["name"] for entry in table["codes"]]
    assert len(names) == 24 and len(set(names)) == 24
    assert set(documented) == set(names)
    for entry in table["codes"]:
        assert documented[entry["name"]] == (entry["exitCode"], entry["action"])
        assert 1 <= entry["exitCode"] <= 9
    assert documented["protocol_version_incompatible"] == (9, "upgrade")
    assert documented["unavailable"] == (5, "retry")


def _wire_routes() -> list[str]:
    text = WIRE.read_text(encoding="utf-8")
    block = text.split("## HTTP surface", 1)[1].split("```http", 1)[1].split("```", 1)[0]
    routes = []
    for line in block.strip().splitlines():
        parts = line.split()
        if len(parts) >= 2:
            routes.append(f"{parts[0]} {parts[1]}")
    return routes


def _wire_topics() -> list[str]:
    text = WIRE.read_text(encoding="utf-8")
    block = text.split("## Topics", 1)[1].split("```text", 1)[1].split("```", 1)[0]
    return [line.strip() for line in block.strip().splitlines() if line.strip()]


@pytest.mark.description(
    "When the contract surface snapshot is compared with WIRE.md and the generated descriptor, heartbeat, routes, topics and type names cannot drift."
)
def test_contract_surface_snapshot():
    surface = json.loads(SURFACE.read_text(encoding="utf-8"))
    assert surface["schema"] == "whalerein/proto-surface/v1"
    assert surface["httpRoutes"] == _wire_routes()
    assert "POST /v1/requests/{request_id}/heartbeat" in surface["httpRoutes"]
    assert surface["mqttTopics"] == _wire_topics()
    assert surface["zeroValuePolicy"] == "omitted"
    assert surface["int64JsonEncoding"] == "decimal-string"
    assert surface["timeUnit"] == "unix-epoch-milliseconds"
    assert surface["protocolWindow"] == {"min": 1, "max": 1}

    descriptor = json.loads(
        (ROOT / "host/runtime/src/generated/whalerein.json").read_text(encoding="utf-8")
    )
    messages = descriptor["nested"]["whalerein"]["nested"]["v1"]["nested"]
    schema_messages = sorted(
        f"whalerein.v1.{name}"
        for name, body in messages.items()
        if "fields" in body
    )
    schema_enums = sorted(
        f"whalerein.v1.{name}"
        for name, body in messages.items()
        if "values" in body
    )
    assert surface["messages"] == schema_messages
    assert surface["enums"] == schema_enums

    errors = [entry["name"] for entry in json.loads(ERROR_CODES.read_text())["codes"]]
    assert surface["errorCodes"] == errors
    assert set(surface["harnessTypes"]) >= {"Message", "RuntimeState", "ApprovalState", "HistoryReset"}


@pytest.mark.description(
    "When the Swift surface classifier runs, every wire code is classified and upgrade-critical follow-ups are named instead of degrading silently."
)
def test_swift_surface_classification():
    result = run([sys.executable, str(SWIFT_CHECK)])
    assert result.returncode == 0, result.stderr[-2000:]
    assert "Swift surface drift check passed" in result.stdout
    surface = json.loads(
        (ROOT / "proto/v1/bindings/swift/ios-surface.json").read_text(encoding="utf-8")
    )
    follow_ups = {
        name for name, entry in surface["errorCodes"].items() if entry["status"] == "follow-up"
    }
    codes = json.loads(ERROR_CODES.read_text(encoding="utf-8"))["codes"]
    upgrade = {entry["name"] for entry in codes if entry["action"] == "upgrade"}
    # The app now has distinct non-retryable upgrade cases, so these are mapped
    # rather than silently degrading to a retryable unavailable error.
    assert "protocol_version_incompatible" not in follow_ups
    assert "unsupported_capability" not in follow_ups
    for name in ("protocol_version_incompatible", "unsupported_capability"):
        assert surface["errorCodes"][name]["status"] == "mapped"
    assert upgrade <= set(surface["errorCodes"])


@pytest.mark.description(
    "When a peer omits protocol_version, the version window treats it as an older peer instead of a wildcard."
)
def test_protocol_window_is_closed():
    surface = json.loads(SURFACE.read_text(encoding="utf-8"))
    assert surface["protocolWindow"] == {"min": 1, "max": 1}
    assert not re.search(r"protocol_version[^\n]*\b0\b[^\n]*compatible", WIRE.read_text(encoding="utf-8"))
