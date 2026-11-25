"""The native runtime must load the mounted worktree, not a cached bundle."""

import json
from pathlib import Path

import pytest

from environment import ROOT, wait
from native_environment import source_digest

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_native_bundle_loads_the_mounted_worktree_sources(scenario):
    """Provenance ties every native result to the exact mounted sources."""
    provenance = scenario.provenance
    for name in ("host/runtime/src", "host/dsh/src"):
        assert provenance["hashes"][name] == source_digest(Path(ROOT) / name), name
    for entry in ("driverEntry", "hostRuntime", "dshAdapter"):
        assert provenance[entry]["insideWorkspace"] is True, provenance[entry]
    assert provenance["versions"]["dsh"], "the loaded DSH version must be recorded"

    counters_path = scenario.directory / "plugin" / "native-counters.json"

    def attached():
        try:
            return json.loads(counters_path.read_text()).get("attached") is True
        except (OSError, ValueError):
            return False

    wait(attached, timeout=15)
    counters = json.loads(counters_path.read_text())
    assert isinstance(counters["events"], dict)
    assert counters["live"] == {"attempts": 0, "accepted": 0, "rejected": 0}
    assert "framePhases" in counters