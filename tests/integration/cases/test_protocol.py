"""Protocol integration cases, collected independently by pytest."""

import json
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.protocol

ROOT = Path(__file__).resolve().parents[3]
VECTORS = json.loads((ROOT / "tests/fixtures/proto/messages.json").read_text())[
    "messages"
]
VECTOR_IDS = [vector["caseId"] for vector in VECTORS]
if (
    not VECTOR_IDS
    or any(not isinstance(identity, str) or not identity for identity in VECTOR_IDS)
    or len(VECTOR_IDS) != len(set(VECTOR_IDS))
):
    raise ValueError("Protocol vectors require nonempty, unique caseId values")


def check(command):
    """Run a bounded protocol peer without exposing fixture payloads on failure."""
    result = subprocess.run(
        command, cwd=ROOT, capture_output=True, timeout=300, check=False
    )
    exit_code = result.returncode
    assert exit_code == 0, "Protocol peer rejected the fixture"


@pytest.mark.parametrize(
    "vector,language",
    [
        pytest.param(
            vector,
            language,
            id=f"{vector['caseId']}-{language}",
            marks=pytest.mark.description(vector["description"].format(language=label)),
        )
        for vector in VECTORS
        for language, label in (("node", "Node.js"), ("rust", "Rust"))
    ],
)
def test_protobuf_vector(language, vector, tmp_path):
    """Matches the committed protobuf bytes and JSON for this implementation and vector."""
    fixture = tmp_path / "vector.json"
    fixture.write_text(
        json.dumps({"schema": "whalerein/proto-json/v1", "messages": [vector]})
    )
    peer = ROOT / "tests/integration/drivers/protobuf"
    command = (
        ["node", str(peer / "node_peer.mjs"), "--check"]
        if language == "node"
        else [
            str(peer / "rust-peer/target/debug/whalerein-proto-peer"),
        ]
    )
    check([*command, str(fixture)])


@pytest.mark.parametrize(
    "language",
    [
        pytest.param(
            language,
            marks=pytest.mark.description(
                f"When {label} performs the fixed Noise XX exchange, handshake bytes, derived keys and tamper rejection match the committed transcript."
            ),
        )
        for language, label in (("node", "Node.js"), ("rust", "Rust"))
    ],
)
def test_noise_transcript(language):
    """Matches the fixed Noise transcript, derived keys, and tamper checks."""
    peer = ROOT / "tests/integration/drivers/noise"
    command = (
        ["node", str(peer / "node_peer.mjs"), "--check"]
        if language == "node"
        else [
            str(peer / "rust-peer/target/debug/whalerein-noise-peer"),
        ]
    )
    check([*command, str(ROOT / "tests/fixtures/noise/xx-transcript.json")])


def test_javascript_descriptor():
    """When the wire schema is loaded, its field signatures match the committed JavaScript descriptor."""
    check(
        [
            "node",
            str(ROOT / "tests/integration/drivers/protobuf/node_peer.mjs"),
            "--self-descriptor-check",
        ]
    )


def test_field_contract():
    """When the current schema is compiled, message fields and enum values match the committed contract."""
    import sys

    check(
        [
            sys.executable,
            str(ROOT / "tests/integration/drivers/protobuf/proto_compat.py"),
            "--check",
        ]
    )
