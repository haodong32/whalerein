"""Native Xcode verification with isolated fixture state and simulator leasing."""

from __future__ import annotations

import hashlib
import json
import os
import plistlib
import re
import shutil
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path

from ..sources import SourceSelection
from ..verification import Invocation, VerificationError, run_suite

PLANS = {"ios-unit": "Unit", "ios-ui": "UI", "ios-ui-smoke": "UI", "ios-integration": "Integration"}
SMOKE_CASES = {
    "NavigationAndConversationUITests/testPairingToConversation",
    "NavigationAndConversationUITests/testSendWhileIdleAndWhileRunning",
    "ApprovalUITests/testAllowOnceUpdatesOriginalCard",
    "LifecycleAndRecoveryUITests/testRemoteRevocationBlocksConversationAndPairsAgain",
    "LifecycleAndRecoveryUITests/testWatchEndedDisablesSendAndReconnects",
    "AdaptiveLayoutUITests/testLandscapeShowsSidebarAndSwitchesConversation",
}
STACK_FIELDS = (
    "access_url",
    "mqtt_url",
    "control_url",
    "control_token",
    "host_id",
    "session_id",
    "teardown_marker",
)


def _json(result, failure):
    if not result.ok:
        raise VerificationError(failure)
    try:
        return json.loads(result.stdout)
    except ValueError as exc:
        raise VerificationError(failure) from exc


def _simulator(ctx, runner, root, selected):
    devices = _json(
        ctx.run(runner, ["xcrun", "simctl", "list", "devices", "available", "-j"], root),
        "simulator-inventory-failed",
    )["devices"]
    available = [
        device for group in devices.values() for device in group if device.get("isAvailable")
    ]
    if selected:
        matches = [device for device in available if device["udid"].lower() == selected.lower()]
        if len(matches) != 1:
            raise VerificationError("unknown-simulator", 2)
        return matches[0]
    phones = [device for device in available if device["name"].startswith("iPhone")]
    booted = [device for device in phones if device["state"] == "Booted"]
    if len(booted) == 1:
        return booted[0]
    if len(phones) == 1:
        return phones[0]
    raise VerificationError("select-simulator-udid", 3)


def _lease(ctx, udid):
    import fcntl

    if not re.fullmatch(r"[a-fA-F0-9-]{36}", udid):
        raise VerificationError("invalid-simulator-udid", 2)
    directory = Path.home() / "Library" / "Caches" / "whalerein" / "simulator-leases"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    handle = (directory / f"{udid.lower()}.lock").open("a")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as exc:
        handle.close()
        raise VerificationError("simulator-leased", 3) from exc
    ctx.register_cleanup("simulator-lease", lambda deadline: handle.close())
    ctx.record_resource(
        "simulator",
        {
            "kind": "simulator-lease",
            "udid": udid,
            "lease": ctx.run_directory.name,
        },
    )


def _nodes(document):
    if isinstance(document, list):
        for node in document:
            yield from _nodes(node)
    elif isinstance(document, dict):
        if document.get("nodeType") == "Test Case":
            yield document
        yield from _nodes(document.get("children", []))


def _enumerated(document):
    def visit(node, parents):
        kind = node["kind"]
        parts = parents if kind == "plan" else (*parents, node["name"])
        if kind == "test":
            yield "/".join(parts)
        for child in node.get("children", []):
            yield from visit(child, parts)

    for entry in document.get("values", []):
        yield from visit(entry, ())


def _test_id(node):
    url = node.get("nodeIdentifierURL", "")
    path = url.partition("/WhaleRein/")[2]
    if path:
        return path
    raise VerificationError("xctest-case-id-missing")


def _write_contract(pristine, private, variables):
    plist = plistlib.loads(pristine.read_bytes())
    configurations = plist.get("TestConfigurations", [plist])
    targets = [
        target for configuration in configurations for target in configuration["TestTargets"]
    ]
    if not targets:
        raise VerificationError("missing-xctest-target")
    for target in targets:
        target.setdefault("EnvironmentVariables", {}).update(variables)
    fd = os.open(private, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as handle:
        plistlib.dump(plist, handle)


def _broker_tag(source):
    paths = sorted(
        [*source.glob("deploy/mosquitto/**/*"), *source.glob("deploy/docker/mosquitto*")]
    )
    digest = hashlib.sha256()
    for path in paths:
        if path.is_file():
            digest.update(os.fsencode(path.relative_to(source)) + b"\0" + _digest(path).encode())
    return f"whalerein-ios-broker:{digest.hexdigest()}"


def _stack_root():
    # The Docker daemon must share the directory with the native macOS stack.
    # Keep the path short enough for the fixture's Unix-domain sockets.
    return Path.home().resolve() / ".whalerein-ios-runs"


def _start_stack(ctx, runner, source, revision):
    stack_root = _stack_root()
    stack_root.mkdir(exist_ok=True, mode=0o700)
    short = Path(tempfile.mkdtemp(prefix="wr-ios-", dir=stack_root))
    state_dir = short / "stack"
    prefix = [sys.executable, str(source / "tools/ios-stack/ios_stack.py")]
    ownership = {"kind": "ios-stack", "path": str(state_dir)}
    ctx.register_cleanup(
        "ios-stack",
        lambda deadline: _stop_stack(
            runner,
            source,
            "ios-stack",
            ownership,
            deadline,
        ),
    )
    ctx.record_resource("ios-stack", ownership)
    broker = _broker_tag(source)
    contract = _json(
        ctx.run(
            runner,
            [*prefix, "up", "--state", str(state_dir)],
            source,
            env={"WHALEREIN_IOS_BROKER_IMAGE": broker},
        ),
        "ios-stack-start-failed",
    )
    status = _json(
        ctx.run(runner, [*prefix, "status", "--state", str(state_dir), "--json"], source),
        "ios-stack-not-ready",
    )
    ctx.record_identity("ios-stack", contract["run_id"], status["run_id"])
    broker_image = ctx.run(runner, ["docker", "image", "inspect", "-f", "{{.Id}}", broker], source)
    if not broker_image.ok:
        raise VerificationError("ios-broker-image-unavailable")
    runtime_artifacts = status["artifacts"]
    ctx.record_identity("ios-broker", broker_image.stdout.strip(), runtime_artifacts["broker"])
    ctx.record_identity(
        "ios-access",
        _digest(source / ".data/ios-stack/bin/whalerein-access"),
        runtime_artifacts["access"],
    )
    ctx.record_artifact("ios-stack", runtime_artifacts)
    ownership["instance"] = contract["run_id"]
    ctx.record_resource("ios-stack", ownership)
    invitation = ctx.run(
        runner,
        [*prefix, "invitation", "--state", str(state_dir), "--refresh"],
        source,
    )
    if not invitation.ok or not invitation.stdout.strip():
        raise VerificationError("ios-stack-invitation-failed")
    return {
        **{f"WHALEREIN_IOS_STACK_{key.upper()}": contract[key] for key in STACK_FIELDS},
        "WHALEREIN_IOS_STACK_INVITATION_URI": invitation.stdout.strip(),
        "WHALEREIN_IOS_STACK_EXPECTED_REVISION": revision,
    }


@contextmanager
def _build_workspace(ctx, runner, root):
    import fcntl

    workspace = root / ".data" / "ios-build"
    workspace.mkdir(parents=True, exist_ok=True)
    with (workspace / "lock").open("a") as handle:
        with ctx.phase("lock-wait"):
            while True:
                try:
                    fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    time.sleep(min(0.1, ctx.remaining()))
        with ctx.phase("environment-preparation"):
            source = ctx.sources["public"].root
            mirror = workspace / "source"
            mirror.mkdir(exist_ok=True)
            result = ctx.run(
                runner,
                [
                    "rsync",
                    "-rlc",
                    "--delete",
                    "--exclude=/target/",
                    "--exclude=/apps/ios/Vendor/",
                    str(source) + "/",
                    str(mirror) + "/",
                ],
                root,
            )
            if not result.ok:
                raise VerificationError("ios-source-mirror-failed")
            # macOS openrsync does not apply --chmod consistently. Preserve
            # executable bits without changing mtimes for identical files.
            for original in source.rglob("*"):
                if original.is_file() and not original.is_symlink():
                    copy = mirror / original.relative_to(source)
                    mode = (original.stat().st_mode & 0o777) | 0o200
                    if copy.stat().st_mode & 0o777 != mode:
                        copy.chmod(mode)
        yield mirror, workspace / "derived-data"


def _digest(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def _product_digests(products):
    identities = {}
    for info in products.rglob("Info.plist"):
        if info.parent.suffix not in {".app", ".xctest", ".framework"}:
            continue
        bundle = plistlib.loads(info.read_bytes())
        executable = info.parent / bundle["CFBundleExecutable"]
        if not executable.is_file() or not executable.resolve().is_relative_to(products):
            raise VerificationError("missing-xcode-bundle-executable")
        identities[executable.relative_to(products).as_posix()] = _digest(executable)
    if not identities:
        raise VerificationError("empty-xcode-products")
    return identities


def _build(ctx, runner, root, captured, plan, destination):
    with _build_workspace(ctx, runner, root) as (source, derived):
        ios = source / "apps" / "ios"
        with ctx.phase("compilation"):
            binding = ctx.run(
                runner,
                ["bash", "apps/mobile-bindings/scripts/build-xcframework.sh"],
                source,
                env={
                    "WHALEREIN_MOBILE_SOURCE": captured.fingerprint,
                    "WHALEREIN_MOBILE_SIMULATOR_ONLY": "1",
                },
            )
            if not binding.ok:
                raise VerificationError("mobile-bindings-build-failed")
            revision = (ios / "Vendor" / "revision.txt").read_text().strip()
            ctx.record_artifact("mobile-bindings", revision)
            if not revision.startswith(f"source:{captured.fingerprint} "):
                raise VerificationError("mobile-bindings-identity-mismatch")
            result = ctx.run(
                runner,
                [
                    "xcodebuild",
                    "build-for-testing",
                    "-project",
                    str(ios / "WhaleRein.xcodeproj"),
                    "-scheme",
                    "WhaleRein",
                    "-destination",
                    destination,
                    "-testPlan",
                    plan,
                    "-derivedDataPath",
                    str(derived),
                ],
                source,
            )
            if not result.ok:
                raise VerificationError("xcode-build-failed")
        with ctx.phase("artifact-publication"):
            products = derived / "Build" / "Products"
            runs = list(products.glob(f"WhaleRein_{plan}_*.xctestrun"))
            if len(runs) != 1:
                raise VerificationError("ambiguous-xctestrun")
            published = ctx.run_directory / "products"
            copy = ctx.run(runner, ["cp", "-cR", str(products), str(published)], root)
            if not copy.ok:
                raise VerificationError("xctest-publication-failed")
            pristine = published / runs[0].name
            ctx.record_artifact(
                "xctestrun",
                {
                    "sha256": _digest(pristine),
                    "source": captured.fingerprint,
                },
            )
            ctx.record_artifact(
                "xcode-products",
                {
                    "source": captured.fingerprint,
                    "binaries": _product_digests(published),
                },
            )
    return pristine, revision


def _run_platform(ctx, runner, root, target, simulator, shard):
    if sys.platform != "darwin":
        raise VerificationError("requires-macos", 3)
    plan = PLANS[target]
    captured = ctx.sources["public"]
    with ctx.phase("platform-preparation"):
        for tool in ("xcodebuild", "xcrun", "rsync", "cargo", "rustup", "lipo", "protoc"):
            runner.require(tool)
        if target == "ios-integration":
            for tool in ("docker", "go", "node", "pnpm"):
                runner.require(tool)
            if not ctx.run(runner, ["docker", "info", "--format", "{{.Architecture}}"], root).ok:
                raise VerificationError("docker-unavailable", 3)
        device = _simulator(ctx, runner, root, simulator)
        udid = device["udid"]
        with ctx.phase("lock-wait"):
            _lease(ctx, udid)
    destination = f"platform=iOS Simulator,id={udid}"
    pristine, revision = _build(ctx, runner, root, captured, plan, destination)
    source = captured.root
    command = [
        "xcodebuild",
        "test-without-building",
        "-destination",
        destination,
        "-parallel-testing-enabled",
        "NO",
    ]
    with ctx.phase("collection"):
        inventory_path = ctx.run_directory / "xctest-inventory.json"
        result = ctx.run(
            runner,
            [
                *command,
                "-xctestrun",
                str(pristine),
                "-enumerate-tests",
                "-test-enumeration-format",
                "json",
                "-test-enumeration-output-path",
                str(inventory_path),
            ],
            source,
        )
        if not result.ok or not inventory_path.is_file():
            raise VerificationError("xctest-inventory-failed")
        inventory = set(_enumerated(json.loads(inventory_path.read_text())))
        selected = sorted(
            identifier
            for identifier in inventory
            if ctx.invocation.case_filter is None or ctx.invocation.case_filter in identifier
        )
        if target == "ios-ui-smoke":
            selected = [
                case for case in selected if case.split("/", 1)[1].removesuffix("()") in SMOKE_CASES
            ]
            if len(selected) != len(SMOKE_CASES):
                raise VerificationError("incomplete-ios-smoke-inventory")
        if shard is not None:
            ctx.record_artifact(
                "xctest-inventory",
                {
                    "count": len(inventory),
                    "sha256": hashlib.sha256("\n".join(sorted(inventory)).encode()).hexdigest(),
                },
            )
            index, count = shard
            selected = selected[index - 1 :: count]
            if not selected:
                raise VerificationError("empty-ios-ui-shard", 2)
        if not selected:
            if inventory:
                raise VerificationError("unmatched-case-filter", 2)
            raise VerificationError("empty-xctest-inventory")
        ctx.record_collection(selected)
        if ctx.invocation.configuration["list_only"]:
            ctx.record_scope(
                {
                    "suite_id": target,
                    "project": target,
                    "kind": "integration",
                    "language": "swift",
                    "completeness": "complete",
                    "exit_code": 0,
                    "tests": [{"id": case, "status": "todo"} for case in selected],
                }
            )
            return
    test_run = pristine
    if target == "ios-integration":
        with ctx.phase("readiness"):
            source = ctx.run_directory / "ios-build"
            shutil.copytree(captured.root, source, symlinks=True)
            for path in [source, *source.rglob("*")]:
                if not path.is_symlink():
                    path.chmod(path.stat().st_mode | 0o200)
            install = ctx.run(
                runner,
                ["pnpm", "install", "--frozen-lockfile", "--ignore-scripts"],
                source,
                env={"CI": "true"},
            )
            if not install.ok:
                raise VerificationError("ios-stack-node-dependencies-failed")
            environment = _start_stack(ctx, runner, source, revision)
            test_run = pristine.with_name("verification.xctestrun")
            ctx.register_cleanup(
                "xctest-contract",
                lambda deadline: test_run.unlink(missing_ok=True),
            )
            ctx.record_resource(
                "xctest-contract",
                {
                    "kind": "xctest-contract",
                    "path": str(test_run.relative_to(ctx.run_directory)),
                },
            )
            _write_contract(pristine, test_run, environment)
    bundle = ctx.run_directory / "native.xcresult"
    only_cases = (
        [f"-only-testing:{identifier}" for identifier in selected]
        if ctx.invocation.case_filter or target == "ios-ui-smoke" or shard is not None
        else []
    )
    with ctx.phase("execution"):
        run = runner.run(
            [
                *command,
                "-xctestrun",
                str(test_run),
                "-test-timeouts-enabled",
                "YES",
                "-default-test-execution-time-allowance",
                "180",
                "-resultBundlePath",
                str(bundle),
                *only_cases,
            ],
            source,
            timeout=ctx.remaining(),
            quiet=True,
        )
        observed = {}
        if bundle.is_dir():
            result = runner.run(
                [
                    "xcrun",
                    "xcresulttool",
                    "get",
                    "test-results",
                    "tests",
                    "--path",
                    str(bundle),
                ],
                source,
                timeout=15,
                quiet=True,
            )
            if result.ok:
                document = _json(result, "xctest-result-invalid")
                actual_devices = sorted({device["deviceId"] for device in document["devices"]})
                ctx.record_identity("xctest-simulator", [udid], actual_devices)
                for node in _nodes(document.get("testNodes", [])):
                    identifier = _test_id(node)
                    # XCTest drops the trailing parentheses in result URLs;
                    # Swift Testing keeps them. The enumeration defines the scope.
                    if identifier not in inventory and identifier + "()" in inventory:
                        identifier += "()"
                    outcomes = {"Passed": "passed", "Skipped": "skipped"}
                    status = outcomes.get(node.get("result"), "failed")
                    observed[identifier] = status
                    ctx.record_case(identifier, status)
        complete = set(observed) == set(selected)
        ctx.record_scope(
            {
                "suite_id": target,
                "project": target,
                "kind": "integration",
                "language": "swift",
                "completeness": "complete" if complete else "incomplete",
                "exit_code": run.returncode,
                "tests": [
                    {"id": case, "status": observed.get(case, "not_run")} for case in selected
                ],
            }
        )
        if run.returncode == 130:
            raise KeyboardInterrupt
        if not complete:
            raise VerificationError("incomplete-xctest-result")
        if not run.ok:
            raise VerificationError("xctest-failed")
        if not any(status == "passed" for status in observed.values()):
            raise VerificationError("no-xctest-case-passed")


def run_ios(
    runner,
    root,
    target,
    case_filter=None,
    output=None,
    simulator=None,
    *,
    list_only=False,
    shard=None,
):
    ci = os.environ.get("GITHUB_ACTIONS") == "true"
    return run_suite(
        Invocation(
            "ci" if ci else "local",
            target,
            (SourceSelection("public", root, revision="HEAD" if ci else None),),
            case_filter=case_filter,
            output=output,
            timeout=3600,
            configuration={"list_only": list_only, "shard": shard},
        ),
        lambda ctx: _run_platform(ctx, runner, root, target, simulator, shard),
    )


def _stop_stack(runner, source, name, ownership, deadline):
    state = Path(ownership.get("path", ""))
    if (
        name != "ios-stack"
        or state.name != "stack"
        or state.parent.parent != _stack_root()
        or not re.fullmatch(r"wr-ios-[a-z0-9_]{8}", state.parent.name)
        or state.parent.is_symlink()
    ):
        raise VerificationError("invalid-ios-stack-ownership")
    if not state.parent.exists():
        return
    if state.parent.stat().st_uid != os.getuid() or state.is_symlink():
        raise VerificationError("ios-stack-ownership-conflict")
    token = state / "run.marker"
    if ownership.get("instance") and (
        not token.is_file() or token.read_text().strip() != ownership["instance"]
    ):
        raise VerificationError("ios-stack-ownership-conflict")
    result = runner.run(
        [
            sys.executable,
            str(source / "tools/ios-stack/ios_stack.py"),
            "down",
            "--state",
            str(state),
        ],
        source,
        timeout=max(0.1, deadline - time.monotonic()),
        quiet=True,
    )
    if not result.ok or not json.loads(result.stdout).get("cleaned"):
        raise VerificationError("ios-stack-cleanup-failed")
    shutil.rmtree(state.parent)


def recovery_callbacks(runner, directory):
    def remove_contract(name, ownership, deadline):
        relative = Path(ownership.get("path", ""))
        path = (directory / relative).resolve()
        if (
            name != "xctest-contract"
            or not path.is_relative_to(directory / "products")
            or path.name != "verification.xctestrun"
        ):
            raise VerificationError("invalid-xctest-contract-owner")
        path.unlink(missing_ok=True)

    def release_lease(name, ownership, deadline):
        # The kernel released this process's advisory lock when it abandoned the run.
        # A newer run may hold the same lock. Recovery must never touch the device.
        if name != "simulator" or ownership.get("lease") != directory.name:
            raise VerificationError("invalid-simulator-lease-owner")

    return {
        "ios-stack": lambda name, ownership, deadline: _stop_stack(
            runner,
            directory / "ios-build",
            name,
            ownership,
            deadline,
        ),
        "xctest-contract": remove_contract,
        "simulator-lease": release_lease,
    }
