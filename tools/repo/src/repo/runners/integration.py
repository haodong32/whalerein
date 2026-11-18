"""Build and execute public Linux suites in labeled, isolated containers."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from time import monotonic, sleep
from uuid import uuid4

from ..errors import RepoError
from ..integration_targets import (
    collection_paths,
    select_leaves,
    selected_artifacts,
    validate_inventory,
)
from ..reports.model import validate_scope
from ..sources import SourceSelection
from ..verification import Invocation, VerificationError, run_suite

DEFAULT_RUN_TIMEOUT_SECONDS = 7200
OWNER_LABEL = "org.whalerein.verification.run"
PLATFORM_LABEL = "org.whalerein.verification.platform-inputs"


def collect(root, runner, request, directory):
    """Dispatch the public testing CLI through the shared lifecycle."""
    target = request.group or "integration"
    try:
        paths = collection_paths(target)
    except RepoError as exc:
        raise VerificationError("unknown-integration-target", 2) from exc

    def platform(context):
        run_platform(context, runner, request, paths)

    timeout = request.integration_timeout or DEFAULT_RUN_TIMEOUT_SECONDS
    formal = os.environ.get("GITHUB_ACTIONS") == "true"
    return run_suite(
        Invocation(
            purpose="ci" if formal else "local",
            suite=target,
            sources=(SourceSelection("public", root, revision="HEAD" if formal else None),),
            case_filter=request.only,
            timeout=timeout,
            output=directory,
            configuration={
                "suite": "integration",
                "group": request.group,
                "only": request.only,
                "parallel": request.parallel or 1,
                "list_only": request.list_only,
                "integration_timeout": timeout,
            },
        ),
        platform,
    )


def _mount(host, guest, *, readonly=False):
    return ["--mount", f"type=bind,src={host},dst={guest}" + (",readonly" if readonly else "")]


def _command(context, runner, argv, cwd, code, *, blocked=False, env=None):
    result = context.run(runner, argv, cwd, env=env)
    if not result.ok:
        # Command output can include product data; keep stable failure codes.
        raise VerificationError(code, 3 if blocked else 1)
    return result.stdout.strip()


def _platform_key(source, target, platform):
    # Docker receives only the literal allowlist in this file. Product and
    # test edits are deliberately absent from the key.
    dockerignore = source / "tests/integration/Dockerfile.dockerignore"
    manifest = dockerignore.read_bytes()
    lines = manifest.decode().splitlines()
    if not lines or lines[0] != "**":
        raise VerificationError("invalid-platform-context")
    digest = hashlib.sha256(f"whalerein-platform-v1:{target}:{platform}".encode())
    digest.update(manifest)
    for line in lines[1:]:
        if not line or line.startswith("#"):
            continue
        path = Path(line.removeprefix("!"))
        if (
            not line.startswith("!")
            or path.is_absolute()
            or ".." in path.parts
            or any(character in str(path) for character in "*?[]\\")
        ):
            raise VerificationError("invalid-platform-context")
        file = source / path
        if file.is_symlink() or not file.is_file():
            raise VerificationError("missing-platform-input")
        digest.update(hashlib.sha256(file.read_bytes()).digest())
        digest.update(bytes([file.stat().st_mode & 0o111]))
    return digest.hexdigest()


def _inspect_platform(context, runner, source, tag, key, platform):
    result = context.run(
        runner,
        [
            "docker",
            "image",
            "inspect",
            "--format",
            '{{.Os}}/{{.Architecture}} {{.Id}} {{index .Config.Labels "' + PLATFORM_LABEL + '"}}',
            tag,
        ],
        source,
    )
    if not result.ok:
        return None
    actual, _, facts = result.stdout.strip().partition(" ")
    if actual != platform:
        raise VerificationError("platform-image-native-mismatch")
    image, _, label = facts.partition(" ")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
        raise VerificationError("invalid-image-identity")
    if label != key:
        raise VerificationError("platform-image-input-mismatch")
    return image


def _build_target(context, runner, source, target, platform):
    key = _platform_key(source, target, platform)
    tag = f"whalerein-verification-platform:{target}-{key}"
    image = _inspect_platform(context, runner, source, tag, key, platform)
    reused = image is not None
    if image is None:
        _command(
            context,
            runner,
            [
                "docker",
                "buildx",
                "build",
                "--load",
                "--progress=plain",
                "--platform",
                platform,
                "--target",
                target,
                "--label",
                f"{PLATFORM_LABEL}={key}",
                "--tag",
                tag,
                "-f",
                str(source / "tests/integration/Dockerfile"),
                str(source),
            ],
            source,
            "linux-platform-build-failed",
        )
        image = _inspect_platform(context, runner, source, tag, key, platform)
        if image is None:
            raise VerificationError("linux-platform-image-unavailable")
    context.record_artifact(
        f"platform-{target}",
        {"digest": image, "image_id": image, "inputs": key, "platform": platform, "reused": reused},
    )
    return image


@dataclass
class _Container:
    context: object
    runner: object
    cwd: Path
    name: str
    label: str
    image: str
    identity: str | None = None
    running: bool = False
    removed: bool = False

    def create(self, options, command):
        # Register intent before creation. Cleanup can acquire the exact
        # identity by name and label if create completed during cancellation.
        self.context.record_resource(self.name, {"kind": "docker-container", "label": self.label})
        self.context.register_cleanup(self.name, self.cleanup)
        identity = _command(
            self.context,
            self.runner,
            [
                "docker",
                "create",
                "--init",
                "--name",
                self.name,
                "--label",
                f"{OWNER_LABEL}={self.label}",
                *options,
                self.image,
                *command,
            ],
            self.cwd,
            "docker-container-create-failed",
            # An ambient client override cannot select another platform for
            # the already proven native image ID.
            env={"DOCKER_DEFAULT_PLATFORM": ""},
        )
        if not re.fullmatch(r"[0-9a-f]{64}", identity):
            raise VerificationError("invalid-container-identity")
        self.identity = identity
        self.context.record_resource(
            self.name, {"kind": "docker-container", "id": identity, "label": self.label}
        )
        if self.inspect(self.context.remaining()) != self.image:
            raise VerificationError("runtime-identity-mismatch")

    def inspect(self, timeout):
        if timeout <= 0:
            raise VerificationError("docker-cleanup-deadline")
        result = self.runner.run(
            [
                "docker",
                "container",
                "inspect",
                "--format",
                '{{.Id}} {{index .Config.Labels "'
                + OWNER_LABEL
                + '"}} {{.Image}} {{.State.Running}}',
                self.identity or self.name,
            ],
            self.cwd,
            timeout=min(timeout, 10),
            quiet=True,
        )
        if not result.ok:
            exists = self.runner.run(
                ["docker", "ps", "-aq", "--no-trunc", "--filter", f"name=^/{self.name}$"],
                self.cwd,
                timeout=min(timeout, 10),
                quiet=True,
            )
            if exists.ok and not exists.stdout.strip():
                return None
            raise VerificationError("docker-ownership-unverified")
        try:
            identity, label, image, running = result.stdout.strip().split()
        except ValueError as exc:
            raise VerificationError("docker-ownership-unverified") from exc
        if (
            not re.fullmatch(r"[0-9a-f]{64}", identity)
            or self.identity not in (identity, None)
            or label != self.label
            or running not in ("true", "false")
        ):
            raise VerificationError("docker-ownership-conflict")
        self.identity = identity
        self.running = running == "true"
        return image

    def execute(self):
        assert self.identity is not None
        return self.runner.run(
            ["docker", "start", "--attach", self.identity],
            self.cwd,
            timeout=self.context.remaining(),
            quiet=True,
        )

    def cleanup(self, deadline):
        if self.removed:
            return
        if self.inspect(deadline - monotonic()) is not None:
            if self.running:
                self.runner.run(
                    ["docker", "stop", "--signal", "SIGINT", "--timeout", "8", self.identity],
                    self.cwd,
                    timeout=min(12, max(1, deadline - monotonic())),
                    quiet=True,
                )
            removal = self.runner.run(
                ["docker", "rm", "--force", self.identity],
                self.cwd,
                timeout=min(15, max(1, deadline - monotonic())),
                quiet=True,
            )
            # The client can reach its wait limit while the daemon still
            # removes the same identified container. Wait only for that
            # in-flight operation within the original cleanup deadline.
            while self.inspect(deadline - monotonic()) is not None:
                if removal.returncode != 124 or monotonic() >= deadline:
                    raise VerificationError("docker-cleanup-unverified")
                sleep(0.2)
        self.removed = True


def _container(context, runner, cwd, image, phase, *, name=None):
    token = uuid4().hex[:16]
    return _Container(context, runner, cwd, name or f"wr-verify-{phase}-{token}", token, image)


def recover_container(runner, cwd, name, ownership, deadline):
    """Release a recorded Linux container after the caller proves the run abandoned."""
    label = ownership.get("label", "")
    identity = ownership.get("id")
    if (
        ownership.get("kind") != "docker-container"
        or not re.fullmatch(
            r"wr-verify-(?:build|mirror|collect|test|benchmark)-[0-9a-f]{16,24}", name
        )
        or not re.fullmatch(r"[0-9a-f]{16}", label)
        or (identity is not None and not re.fullmatch(r"[0-9a-f]{64}", identity))
    ):
        raise VerificationError("invalid-docker-ownership")
    _Container(None, runner, cwd, name, label, "", identity).cleanup(deadline)


def _compile(context, runner, source, worktree, builder, architecture, artifacts, products):
    try:
        import fcntl
    except ImportError as exc:
        raise VerificationError("requires-posix-build-host", 3) from exc

    # The same daemon and worktree share the compiler cache and input mirror.
    # Image IDs can change when BuildKit regenerates attestations. Key the
    # mutable native cache by the compiler inputs and target architecture.
    fingerprint = hashlib.sha256(f"{worktree}:{os.getuid()}:{architecture}".encode())
    for path in (
        "tests/integration/Dockerfile",
        "tools/repo/linux-rust-packages.txt",
        "tools/repo/protoc-linux.sh",
    ):
        fingerprint.update((source / path).read_bytes())
    owner = fingerprint.hexdigest()[:24]
    lock = worktree / ".data" / "locks" / f"verification-{owner}.lock"
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open("a") as handle:
        with context.phase("lock-wait"):
            while True:
                context.remaining()
                try:
                    fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    sleep(0.1)
        build_name = f"wr-verify-build-{owner}"
        with context.phase("build-input-preparation"):
            active = _command(
                context,
                runner,
                ["docker", "ps", "-aq", "--filter", f"name=^/{build_name}$"],
                source,
                "docker-build-ownership-unverified",
            )
            if active:
                raise VerificationError("recover-abandoned-build", 3)
            mirror = _container(
                context, runner, source, builder, "mirror", name=f"wr-verify-mirror-{owner}"
            )
            # Preserve timestamps for equal bytes and advance them for
            # actual changes, including checkouts of older source files.
            mirror.create(
                [
                    "--network",
                    "none",
                    "--user",
                    f"{os.getuid()}:{os.getgid()}",
                    *_mount(source, "/input", readonly=True),
                    "--mount",
                    f"type=volume,src=wr-source-{owner},dst=/mirror",
                ],
                [
                    "bash",
                    "-c",
                    "mkdir -p /mirror/tree && "
                    "exec rsync -rlpc --chmod=Du+rwx,Fu+rw --delete /input/ /mirror/tree/",
                ],
            )
            try:
                result = mirror.execute()
                if result.returncode == 130:
                    raise KeyboardInterrupt
                if not result.ok:
                    raise VerificationError("linux-build-input-preparation-failed")
            finally:
                mirror.cleanup(monotonic() + 40)
        with context.phase("compilation"):
            container = _container(context, runner, source, builder, "build", name=build_name)
            container.create(
                [
                    "--user",
                    f"{os.getuid()}:{os.getgid()}",
                    "--mount",
                    f"type=volume,src=wr-source-{owner},dst=/workspace,volume-subpath=tree,readonly",
                    *_mount(products, "/products"),
                    "--mount",
                    f"type=volume,src=wr-build-{owner},dst=/cache",
                ],
                ["bash", "tests/integration/build-artifacts.sh", *artifacts],
            )
            try:
                result = container.execute()
                if result.returncode == 130:
                    raise KeyboardInterrupt
                if not result.ok:
                    raise VerificationError("linux-compilation-failed")
            finally:
                # Stop compiler readers before releasing the mirror's writer
                # lock, including when docker attach hits its deadline.
                container.cleanup(monotonic() + 40)


def _merge_tree(source, target):
    target.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        destination = target / entry.name
        if entry.is_symlink():
            destination.symlink_to(entry.readlink())
        elif entry.is_dir():
            _merge_tree(entry, destination)
        else:
            try:
                os.link(entry, destination)
            except OSError:
                # Cross-filesystem output and Docker-created files may need
                # copying without weakening source or artifact permissions.
                if destination.exists():
                    raise
                shutil.copy2(entry, destination)


def _product_manifest(products):
    manifest = {}
    for path in sorted(products.rglob("*")):
        relative = path.relative_to(products).as_posix()
        if path.is_symlink():
            manifest[relative] = {"link": str(path.readlink())}
        elif path.is_file():
            with path.open("rb") as stream:
                sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
            manifest[relative] = {"sha256": sha256, "mode": path.stat().st_mode & 0o777}
    return manifest


def _expected_paths(artifacts):
    paths = {
        "access": [".verification/bin/whalerein-access"],
        "broker-plugin": [".verification/lib/whalerein.so"],
        "cli": ["target/debug/whalerein"],
        "noise-peer": [
            "tests/integration/drivers/noise/rust-peer/target/debug/whalerein-noise-peer"
        ],
        "proto-peer": [
            "tests/integration/drivers/protobuf/rust-peer/target/debug/whalerein-proto-peer"
        ],
        "mqtt-driver": ["target/debug/examples/mqtt_transport_driver"],
        "delivery-driver": ["target/debug/examples/delivery_budget_driver"],
        "wait-driver": ["target/debug/examples/wait_concurrency_driver"],
        "broker-tests": [".verification/bin/broker-tests"],
        "server-tests": [".verification/bin/server-tests"],
        "storage-tests": [
            path
            for package in ("store", "store-support", "server", "external")
            for path in (
                f".verification/bin/{package}-tests",
                f"tests/integration/fixtures/{package}-cases.txt",
                f"tests/integration/fixtures/{package}-descriptions.json",
            )
        ],
    }
    return {path for artifact in artifacts for path in paths[artifact]}


def prepare(context, runner, target):
    """Prepare the same source, builder, and artifacts for suites or benchmarks."""
    source = context.sources["public"]
    worktree = context.invocation.sources[0].root
    requirements = selected_artifacts(target)
    artifacts = tuple(artifact for artifact in requirements if artifact != "node-dependencies")
    runtime = (
        "native"
        if "native-dsh" in select_leaves(target)
        else "services"
        if "access" in artifacts
        else "vectors"
    )
    products = context.run_directory / "products"
    products.mkdir()
    with context.phase("environment-preparation"):
        _command(
            context,
            runner,
            ["docker", "buildx", "version"],
            source.root,
            "docker-unavailable",
            blocked=True,
        )
        architecture = _command(
            context,
            runner,
            ["docker", "info", "--format", "{{.OSType}}/{{.Architecture}}"],
            source.root,
            "docker-unavailable",
            blocked=True,
        )
        system, _, architecture = architecture.partition("/")
        native_arch = {"aarch64": "arm64", "x86_64": "amd64"}.get(architecture, architecture)
        if system != "linux" or native_arch not in {"arm64", "amd64"}:
            raise VerificationError("unsupported-docker-platform", 3)
        platform = f"linux/{native_arch}"
        image = _build_target(context, runner, source.root, runtime, platform)
        builder_target = "services-builder" if "access" in artifacts else "vectors-builder"
        builder = _build_target(context, runner, source.root, builder_target, platform)
    _compile(context, runner, source.root, worktree, builder, architecture, artifacts, products)
    with context.phase("artifact-publication"):
        manifest = _product_manifest(products)
        if _expected_paths(artifacts).difference(manifest):
            raise VerificationError("missing-build-artifact")
        metadata = json.dumps(manifest, sort_keys=True)
        (context.run_directory / "artifact-manifest.json").write_text(metadata)
        context.record_artifact(
            "products",
            {
                "digest": hashlib.sha256(metadata.encode()).hexdigest(),
                "source": source.fingerprint,
                "manifest": "artifact-manifest.json",
            },
        )
        workspace = context.run_directory / "workspace"
        _merge_tree(source.root, workspace)
        _merge_tree(products, workspace)
        if "node-dependencies" in requirements:
            for path in (
                Path("node_modules"),
                Path("host/dsh/node_modules"),
                Path("host/runtime/node_modules"),
            ):
                link = workspace / path
                link.parent.mkdir(parents=True, exist_ok=True)
                link.symlink_to(Path("/opt/whalerein-node") / path)
    return image, workspace


def _create_pytest(
    context, runner, image, workspace, export, paths, request, *, list_only=False, shard=None
):
    export.mkdir(parents=True)
    container = _container(context, runner, workspace, image, "collect" if list_only else "test")
    command = [
        "python3",
        "-m",
        "pytest",
        "-p",
        "no:cacheprovider",
        "-c",
        "tests/integration/pytest.ini",
        "--rootdir",
        "tests/integration",
        *paths,
    ]
    if list_only:
        command += ["--collect-only", "-q"]
    if request.group:
        command += ["--partial-scope"]
    if request.only:
        command += ["--only", request.only]
    if shard is not None:
        command += ["--shard", shard]
    container.create(
        [
            "--network",
            "none",
            "--tmpfs",
            "/tmp:rw,exec,size=1024m,mode=1777",
            "--env",
            "WHALEREIN_PROGRESS=events",
            "--env",
            "WHALEREIN_INTEGRATION_SHARED_BROKER",
            *_mount(workspace, "/workspace", readonly=True),
            *_mount(export, "/reports"),
        ],
        command,
    )
    return container


def _read_scope(export, shard=None):
    payload = json.loads((export / "integration.json").read_text())
    if payload["version"] != 1:
        raise ValueError("unsupported integration export")
    native = payload["scope"]
    validate_scope(native)
    if native.get("shard") != shard:
        raise ValueError("unexpected integration shard")
    return native


def _read_observations(context, exports):
    for export in exports:
        try:
            records = (export / "observations.jsonl").read_text().splitlines()
        except FileNotFoundError:
            continue
        for record in records:
            try:
                case = json.loads(record)
                context.record_case(case["id"], case["status"])
            except (ValueError, KeyError):
                continue


def _combine(scopes, expected, errors, started):
    scope = {
        "suite_id": "integration",
        "project": "integration",
        "kind": "integration",
        "tests": [],
        "coverage": [],
        "checks": [],
        "errors": errors,
        "selection": "full",
        "completeness": "complete",
        "exit_code": 0,
        "duration_ms": round((monotonic() - started) * 1000, 3),
    }
    observed = set()
    for native in scopes:
        scope["errors"].extend(native.get("errors", []))
        if native["selection"] == "partial":
            scope["selection"] = "partial"
        if native["completeness"] != "complete":
            scope["completeness"] = "incomplete"
        if native["exit_code"] != 0:
            scope["exit_code"] = 1
        for case in native["tests"]:
            if case["id"] in observed:
                scope["errors"].append("Duplicate integration case identity across shards")
                continue
            observed.add(case["id"])
            scope["tests"].append(case)
    if observed != expected:
        scope["errors"].append("Observed integration cases do not match the collected inventory")
    if scope["errors"]:
        scope["completeness"] = "incomplete"
        scope["exit_code"] = 1
    return scope


def run_platform(context, runner, request, paths):
    target = request.group or "integration"
    with context.phase("scope"):
        validate_inventory(context.sources["public"].root)
    image, workspace = prepare(context, runner, target)
    export_root = context.run_directory / "container"
    with context.phase("collection"):
        listing = export_root / "collection"
        collector = _create_pytest(
            context, runner, image, workspace, listing, paths, request, list_only=True
        )
        result = collector.execute()
        if result.returncode == 130:
            raise KeyboardInterrupt
        if result.returncode == 4:
            raise VerificationError("invalid-integration-selection", 2)
        if not result.ok:
            raise VerificationError("integration-collection-failed")
        collected = _read_scope(listing)
        identities = {case["id"] for case in collected["tests"]}
        if not identities:
            raise VerificationError("invalid-integration-selection", 2)
        context.record_collection(sorted(identities))
        if request.list_only:
            for case in collected["tests"]:
                case["status"] = "todo"
            context.record_scope(collected)
            return

    parallel = request.parallel or 1
    exports = [export_root / str(index) for index in range(parallel)]
    shards = [f"{index}/{parallel}" if parallel > 1 else None for index in range(parallel)]
    with context.phase("readiness"):
        containers = [
            _create_pytest(context, runner, image, workspace, export, paths, request, shard=shard)
            for export, shard in zip(exports, shards, strict=True)
        ]
        for container in containers:
            context.record_identity(container.name, image, container.inspect(context.remaining()))
    started = monotonic()
    with context.phase("execution"):
        scopes, errors = [], []
        interrupted = False
        if parallel == 1:
            results = [containers[0].execute()]
        else:
            display = getattr(runner, "progress", None)
            runner.progress = None
            try:
                with ThreadPoolExecutor(max_workers=parallel) as executor:
                    work = [executor.submit(container.execute) for container in containers]
                    try:
                        results = [future.result() for future in work]
                    except KeyboardInterrupt:
                        interrupted = True
                        deadline = monotonic() + 60
                        for container in containers:
                            try:
                                # Removing also covers prepared containers
                                # whose attach worker has not started yet.
                                container.cleanup(deadline)
                            except VerificationError:
                                pass  # The registered cleanup verifies and reports again.
                        results = [future.result() for future in work]
            finally:
                runner.progress = display
        for result, export, shard in zip(results, exports, shards, strict=True):
            interrupted |= result.returncode == 130
            if result.returncode not in (0, 1):
                errors.append("Integration execution did not finish")
            try:
                scopes.append(_read_scope(export, shard))
            except (OSError, ValueError, RepoError, KeyError, TypeError):
                errors.append("Missing or invalid integration export")
        if errors:
            _read_observations(context, exports)
        combined = _combine(scopes, identities, errors, started)
        context.record_scope(combined)
        if interrupted:
            raise KeyboardInterrupt
        if combined["exit_code"]:
            raise VerificationError("integration-execution-failed")
