"""Environment doctor: report required toolchains without changing anything."""

from __future__ import annotations

import re
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

from .output import redact
from .process import Runner
from .proto import protoc_gen_go_path

_VERSION = re.compile(r"(\d+)(?:\.(\d+))?(?:\.(\d+))?")


@dataclass(frozen=True)
class ToolSpec:
    name: str
    version_args: tuple[str, ...] = ("--version",)
    required: bool = True
    hint: str = ""
    min_version: tuple[int, ...] | None = None


TOOL_SPECS: tuple[ToolSpec, ...] = (
    ToolSpec("go", ("version",), hint="install Go 1.26+", min_version=(1, 26)),
    ToolSpec("cargo", ("--version",), hint="install Rust via rustup"),
    ToolSpec("rustc", ("--version",), hint="install Rust 1.85+ via rustup", min_version=(1, 85)),
    ToolSpec("node", ("--version",), hint="install Node.js 22.5+", min_version=(22, 5)),
    ToolSpec("pnpm", ("--version",), hint="install pnpm"),
    ToolSpec("uv", ("--version",), hint="install uv"),
    ToolSpec("protoc", ("--version",), hint="install protobuf-compiler 3.20+"),
    ToolSpec(
        "protoc-gen-go",
        ("--version",),
        hint="go install google.golang.org/protobuf/cmd/protoc-gen-go@latest",
    ),
    ToolSpec("docker", ("--version",), hint="install Docker"),
    ToolSpec("git", ("--version",), required=False),
)


def _first_line(text: str) -> str:
    for line in text.splitlines():
        stripped = line.strip()
        if stripped:
            return stripped[:200]
    return ""


def parse_version(text: str) -> tuple[int, ...] | None:
    match = _VERSION.search(text)
    if match is None:
        return None
    return tuple(int(part) for part in match.groups() if part is not None)


def version_at_least(found: tuple[int, ...], minimum: tuple[int, ...]) -> bool:
    width = max(len(found), len(minimum))
    padded_found = found + (0,) * (width - len(found))
    padded_minimum = minimum + (0,) * (width - len(minimum))
    return padded_found >= padded_minimum


def _tool_record(
    spec: ToolSpec,
    *,
    found: bool,
    path: str | None,
    version: str | None = None,
    probe_ok: bool = True,
) -> dict:
    parsed = parse_version(version) if version else None
    version_ok = True
    if spec.min_version is not None and parsed is not None:
        version_ok = version_at_least(parsed, spec.min_version)
    return {
        "name": spec.name,
        "required": spec.required,
        "found": found,
        "path": path,
        "version": redact(version) if version else None,
        "probe_ok": probe_ok,
        "version_ok": version_ok,
        "hint": spec.hint,
    }


def check_tool(runner: Runner, spec: ToolSpec, timeout: float = 10.0) -> dict:
    path = runner.which(spec.name)
    if path is None and spec.name == "protoc-gen-go":
        path = protoc_gen_go_path(runner)
    if path is None:
        return _tool_record(spec, found=False, path=None)
    if not spec.version_args:
        return _tool_record(spec, found=True, path=path)
    result = runner.run([path, *spec.version_args], Path.cwd(), timeout=timeout, quiet=True)
    version = _first_line(result.output)
    return _tool_record(
        spec,
        found=True,
        path=path,
        version=version or None,
        probe_ok=result.ok,
    )


def run_doctor(runner: Runner, specs: Sequence[ToolSpec] = TOOL_SPECS) -> dict:
    tools = [check_tool(runner, spec) for spec in specs]
    missing_required = [
        tool["name"]
        for tool in tools
        if tool["required"] and (not tool["found"] or not tool["probe_ok"])
    ]
    outdated = [
        tool["name"]
        for tool in tools
        if tool["required"] and tool["found"] and not tool.get("version_ok", True)
    ]
    missing_optional = [
        tool["name"] for tool in tools if not tool["required"] and not tool["found"]
    ]
    return {
        "ok": not missing_required and not outdated,
        "tools": tools,
        "missing_required": missing_required,
        "outdated": outdated,
        "missing_optional": missing_optional,
    }


def format_doctor(data: dict) -> str:
    lines = [f"{'TOOL':<16} {'STATUS':<8} {'VERSION'}", "-" * 72]
    for tool in data["tools"]:
        if tool["found"] and not tool.get("version_ok", True):
            status = "OLD"
            version = tool.get("version") or "(version unknown)"
        elif tool["found"]:
            status = "ok" if tool.get("probe_ok", True) else "warn"
            version = tool.get("version") or "(version unknown)"
        else:
            status = "MISSING" if tool["required"] else "optional"
            version = ""
        lines.append(f"{tool['name']:<16} {status:<8} {version}")
    if data["missing_required"] or data.get("outdated"):
        lines.append("")
        if data["missing_required"]:
            lines.append("missing required tools: " + ", ".join(data["missing_required"]))
        if data.get("outdated"):
            lines.append("below the required version: " + ", ".join(data["outdated"]))
        lines.append("install them manually; doctor never modifies the system")
    else:
        lines.append("")
        lines.append("all required tools are available")
    if data["missing_optional"]:
        lines.append("optional tools not found: " + ", ".join(data["missing_optional"]))
    return "\n".join(lines)


def specs_for(task):
    base = [spec for spec in TOOL_SPECS if spec.name in ("uv", "git")]
    names = {
        None: ("cargo", "rustc", "go", "node", "pnpm"),
        "check": ("cargo", "rustc", "go", "node", "pnpm"),
        "build": ("cargo", "rustc", "go", "node", "pnpm", "protoc"),
        "test": ("cargo", "rustc", "go", "node", "pnpm", "protoc"),
        "coverage": ("cargo", "rustc", "go", "node", "pnpm", "protoc"),
        "integration": ("docker",),
        "proto": ("protoc", "protoc-gen-go", "node"),
    }[task]
    result = base + [spec for spec in TOOL_SPECS if spec.name in names]
    if task == "integration":
        result.append(ToolSpec("docker", ("buildx", "version")))
    if task == "coverage":
        result.append(ToolSpec("cargo", ("llvm-cov", "--version")))
    return result
