"""Verify the iOS adapter's scope, identity, and private contract handling."""

import hashlib
import json
import os
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from repo.process import ProcessResult
from repo.runners import ios
from repo.verification import VerificationError

pytestmark = pytest.mark.skipif(
    os.name == "nt", reason="iOS orchestration uses POSIX resource leases"
)

DEVICE = "4F88811C-7693-4E14-9CE5-0A92870A580B"
CASE = "WhaleReinTests/TinyTests/checkConnection()"
PLIST = {"TestConfigurations": [{"TestTargets": [{"EnvironmentVariables": {"EXISTING": "yes"}}]}]}


class Xcode:
    def __init__(self, fault, *, names=("checkConnection()",)):
        self.fault = fault
        self.names = names
        self.executed = None
        self.commands = []
        self.seen_contract = None

    def require(self, tool):
        return tool

    def run(self, args, cwd, *, env=None, timeout=None, quiet=False):
        args = tuple(map(str, args))
        self.commands.append(args)
        assert quiet and timeout > 0
        if args[:3] == ("xcrun", "simctl", "list"):
            return self.result(
                {
                    "devices": {
                        "runtime": [
                            {
                                "isAvailable": True,
                                "name": "iPhone 17",
                                "udid": DEVICE,
                                "state": "Booted",
                            }
                        ]
                    }
                }
            )
        if args[0] == "rsync":
            shutil.copytree(args[-2], args[-1], dirs_exist_ok=True)
            Path(args[-1]).chmod(0o755)
            return self.result()
        if args[:2] == ("docker", "info"):
            return self.result(raw="aarch64")
        if args[:3] == ("docker", "image", "inspect"):
            return self.result(raw="sha256:fixture-image")
        if args[0] == "pnpm":
            return self.result()
        if args[0] == "bash":
            assert env["WHALEREIN_MOBILE_SIMULATOR_ONLY"] == "1"
            if self.fault == "compile":
                return self.result(code=1)
            vendor = cwd / "apps/ios/Vendor"
            vendor.mkdir(parents=True, exist_ok=True)
            (vendor / "revision.txt").write_text(
                f"source:{env['WHALEREIN_MOBILE_SOURCE']} core:0.1.0"
            )
            return self.result()
        if args[:2] == ("xcodebuild", "build-for-testing"):
            products = Path(args[args.index("-derivedDataPath") + 1]) / "Build/Products"
            app = products / "WhaleRein.app"
            app.mkdir(parents=True, exist_ok=True)
            (app / "WhaleRein").write_bytes(b"test binary")
            (app / "Info.plist").write_bytes(plistlib.dumps({"CFBundleExecutable": "WhaleRein"}))
            plan = args[args.index("-testPlan") + 1]
            (products / f"WhaleRein_{plan}_device.xctestrun").write_bytes(plistlib.dumps(PLIST))
            return self.result()
        if args[0] == "cp":
            shutil.copytree(args[-2], args[-1])
            return self.result()
        if "-enumerate-tests" in args:
            inventory = {
                "values": [
                    {
                        "kind": "plan",
                        "name": "Integration",
                        "children": [
                            {
                                "kind": "target",
                                "name": "WhaleReinTests",
                                "children": [
                                    {
                                        "kind": "class",
                                        "name": "TinyTests",
                                        "children": [
                                            {"kind": "test", "name": name} for name in self.names
                                        ],
                                    }
                                ],
                            }
                        ],
                    }
                ]
            }
            Path(args[-1]).write_text(json.dumps(inventory))
            return self.result()
        if args[1].endswith("ios_stack.py"):
            state = Path(args[args.index("--state") + 1]).resolve()
            action = args[2]
            if action == "up":
                state.mkdir()
                binary = cwd / ".data/ios-stack/bin/whalerein-access"
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b"test access")
                (state / "run.marker").write_text("instance")
                return self.result(
                    {"run_id": "instance", **dict.fromkeys(ios.STACK_FIELDS, "fixture")}
                )
            if action == "status":
                return self.result(
                    {
                        "run_id": "stale" if self.fault == "stale" else "instance",
                        "artifacts": {
                            "broker": "sha256:fixture-image",
                            "access": hashlib.sha256(b"test access").hexdigest(),
                        },
                    }
                )
            if action == "invitation":
                return self.result(raw="test-invitation")
            if action == "down":
                shutil.rmtree(state)
                return self.result({"cleaned": True})
        if args[:2] == ("xcodebuild", "test-without-building"):
            contract = Path(args[args.index("-xctestrun") + 1])
            if contract.name == "verification.xctestrun":
                assert contract.stat().st_mode & 0o777 == 0o600
                self.seen_contract = contract
                variables = plistlib.loads(contract.read_bytes())["TestConfigurations"][0][
                    "TestTargets"
                ][0]["EnvironmentVariables"]
                assert variables["EXISTING"] == "yes"
                assert variables["WHALEREIN_IOS_STACK_INVITATION_URI"] == "test-invitation"
            self.executed = [
                arg.removeprefix("-only-testing:")
                for arg in args
                if arg.startswith("-only-testing:")
            ]
            if self.fault == "cancel":
                return self.result(code=130)
            Path(args[args.index("-resultBundlePath") + 1]).mkdir()
            return self.result()
        if args[:2] == ("xcrun", "xcresulttool"):
            return self.result(
                {
                    "devices": [{"deviceId": "wrong-device" if self.fault == "device" else DEVICE}],
                    "testNodes": []
                    if self.fault == "missing"
                    else [
                        {
                            "nodeType": "Test Case",
                            "result": "Passed",
                            "nodeIdentifierURL": f"test://com.apple.xcode/WhaleRein/{case.removesuffix('()')}",
                        }
                        for case in self.executed
                    ],
                }
            )
        raise AssertionError("unexpected invocation")

    @staticmethod
    def result(document=None, *, code=0, raw=None):
        return ProcessResult(
            argv=(),
            cwd=".",
            returncode=code,
            stdout=raw if raw is not None else json.dumps(document),
        )


@pytest.fixture
def checkout(tmp_path, monkeypatch):
    root = tmp_path / "checkout"
    root.mkdir()
    (root / "input").write_text("test only")
    for command in [
        ["init"],
        ["add", "input"],
        ["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-m", "base"],
    ]:
        subprocess.run(["git", "-C", root, *command], check=True, capture_output=True)
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr(Path, "home", lambda: tmp_path / "home")
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    return root


@pytest.mark.parametrize(
    ("fault", "exit_code", "error"),
    [
        (None, 0, None),
        ("compile", 1, "mobile-bindings-build-failed"),
        ("unmatched", 2, "unmatched-case-filter"),
        ("stale", 1, "runtime-identity-mismatch"),
        ("device", 1, "runtime-identity-mismatch"),
        ("cancel", 130, "interrupted"),
        ("missing", 1, "incomplete-xctest-result"),
    ],
)
def test_ios_adapter_contract(checkout, tmp_path, fault, exit_code, error):
    xcode = Xcode(fault)
    output = tmp_path / "report"
    result = ios.run_ios(
        xcode,
        checkout,
        "ios-integration",
        case_filter="unmatched" if fault == "unmatched" else "checkConnection",
        output=output,
        simulator=DEVICE,
    )
    facts = (output / "report.json").read_text()
    assert "test-invitation" not in facts
    verification = json.loads(facts)["verification"]
    assert result.exit_code == exit_code
    assert verification["error_code"] == error
    assert verification["cleanup"]["status"] == "complete"
    if fault in {"compile", "unmatched", "stale"}:
        assert xcode.seen_contract is None
        assert not any("-resultBundlePath" in args for args in xcode.commands)
    else:
        assert xcode.seen_contract is not None and not xcode.seen_contract.exists()
    if not fault:
        assert verification["collected_cases"] == [CASE]
        assert xcode.executed == [CASE]
        assert verification["identities"]["xctest-simulator"]["actual"] == [DEVICE]


def test_ios_ui_shards_execute_disjoint_inventory_subsets(checkout, tmp_path):
    names = ("testD()", "testC()", "testE()", "testA()", "testB()")
    observed = []
    inventories = []
    for index in range(1, 3):
        xcode = Xcode(None, names=names)
        result = ios.run_ios(
            xcode,
            checkout,
            "ios-ui",
            output=tmp_path / f"shard-{index}",
            simulator=DEVICE,
            shard=(index, 2),
        )
        assert result.exit_code == 0
        report = json.loads((result.report_directory / "report.json").read_text())
        assert report["configuration"]["shard"] == [index, 2]
        assert xcode.executed == report["verification"]["collected_cases"]
        observed.extend(xcode.executed)
        inventories.append(report["verification"]["artifacts"]["xctest-inventory"])

    assert inventories[0] == inventories[1]
    assert inventories[0]["count"] == len(names)
    assert sorted(observed) == [f"WhaleReinTests/TinyTests/{name}" for name in sorted(names)]


def test_recovery_never_unlinks_a_contract_outside_the_run(tmp_path):
    remove_contract = ios.recovery_callbacks(None, tmp_path)["xctest-contract"]
    foreign = tmp_path / "verification.xctestrun"
    foreign.touch()
    with pytest.raises(VerificationError, match="invalid-xctest-contract-owner"):
        remove_contract("xctest-contract", {"path": str(foreign)}, 0)
    assert foreign.exists()
    products = tmp_path / "products"
    products.mkdir()
    owned = products / foreign.name
    owned.touch()
    remove_contract("xctest-contract", {"path": "products/" + owned.name}, 0)
    assert not owned.exists()


def test_ios_listing_collects_before_starting_fixture(checkout, tmp_path):
    xcode = Xcode(None)
    output = tmp_path / "list"
    result = ios.run_ios(
        xcode,
        checkout,
        "ios-integration",
        output=output,
        simulator=DEVICE,
        list_only=True,
    )
    report = json.loads((output / "report.json").read_text())
    assert result.exit_code == 0
    assert report["scopes"][0]["tests"] == [{"id": CASE, "status": "todo"}]
    assert not any("-resultBundlePath" in args or "up" in args for args in xcode.commands)
