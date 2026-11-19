"""Storage assertions kept in their native Go tests, selected individually."""

import json
import re
import subprocess

import pytest
from environment import ROOT

pytestmark = pytest.mark.storage


def native_cases():
    # The image build records native discovery without executing a case.
    for package in ("store", "store-support", "server", "external"):
        manifest = ROOT / "tests/integration/fixtures" / f"{package}-cases.txt"
        if not manifest.is_file():
            raise RuntimeError(
                "Native integration inventory missing; build the test image"
            )
        descriptions = json.loads(
            manifest.with_name(f"{package}-descriptions.json").read_text()
        )
        for name in manifest.read_text().splitlines():
            if (
                name.startswith("Test")
                and name != "TestIdentityAndEncryptedTransportAgainstIsolatedStack"
            ):
                yield pytest.param(
                    package,
                    name,
                    id=f"{package}.{name}",
                    marks=pytest.mark.description(descriptions[name]),
                )


@pytest.mark.parametrize("package,name", list(native_cases()))
def test_storage(package, name, postgres):
    """Passes the selected native PostgreSQL behavior without skipped assertions."""
    result = subprocess.run(
        [f"/workspace/.verification/bin/{package}-tests", f"-test.run=^{name}$", "-test.v"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=90,
        check=False,
    )
    exit_code = result.returncode
    locations = sorted(set(re.findall(r"[A-Za-z_]+_test.go:\d+", result.stdout)))
    assert exit_code == 0, "Native database integration failed at " + ", ".join(
        locations
    )
    if "SKIP" in result.stdout:
        raise AssertionError("Required database integration was skipped")
