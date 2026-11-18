"""AC-14 artifact conformance for the version-one session observation contract.

These cases run only local Node.js with the committed compiled protobuf
descriptor and canonical vectors. They do not need PostgreSQL, a broker, or a
device. A missing required observation field must stay detectable and invalid;
it must never become an implicit empty Questions collection, an alias reader,
or a hidden second protocol version.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.protocol

ROOT = Path(__file__).resolve().parents[3]
DRIVER = ROOT / "tests/integration/drivers/protobuf/required_fields.mjs"


def check(mode: str) -> None:
    result = subprocess.run(
        ["node", str(DRIVER), mode],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, result.stderr[-2000:]
    assert "artifact-conformance-ok" in result.stdout


@pytest.mark.description(
    "When matching new-contract artifacts communicate, the compiled descriptor accepts WatchSession.interests, an available descriptor stamp, and a stamped complete page with protocol_version 1."
)
def test_matching_required_fields_are_accepted():
    check("--check-matching")


@pytest.mark.description(
    "When an observation omits WatchSession.interests, an available descriptor stamp, or a page stamp/complete marker, the artifact keeps the absence detectable and the published rule fails explicitly instead of producing empty Questions."
)
def test_missing_required_fields_fail_explicitly():
    check("--check-missing")


@pytest.mark.description(
    "When the compiled version-one schema is inspected, it has no compatibility reader, alias watch/page message, or hidden second protocol version, and canonical vectors remain version 1."
)
def test_no_alias_fallback_or_hidden_second_version():
    check("--check-invariants")
