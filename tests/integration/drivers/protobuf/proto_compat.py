#!/usr/bin/env python3
"""Wire-schema compatibility checker for proto/v1/whalerein.proto.

The checker compares the compiled descriptor with two checked-in artifacts:

* ``proto/v1/compat/baseline.json`` is the immutable compatibility baseline.
  ``--check`` fails when a field or enum value is removed, a tag number is
  reused, a field type or JSON name changes, or a field changes oneof
  membership. ``--write`` cannot rewrite it by accident; ``--update-baseline``
  refuses a breaking change unless ``--rationale`` explains the version bump and
  records it in ``proto/v1/compat/CHANGELOG.md``.
* ``tests/fixtures/proto/fields.json`` is an exact snapshot used to detect any
  unreviewed schema edit. ``--write`` refreshes it only after the baseline check
  passes.

Usage:
  proto_compat.py --check [--root DIR]
  proto_compat.py --write [--root DIR]
  proto_compat.py --update-baseline [--rationale TEXT] [--root DIR]
"""

from __future__ import annotations

import argparse
import difflib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

FIXTURE_REL = "tests/fixtures/proto/fields.json"
BASELINE_REL = "proto/v1/compat/baseline.json"
CHANGELOG_REL = "proto/v1/compat/CHANGELOG.md"
PROTO_REL = "proto/v1/whalerein.proto"
BASELINE_SCHEMA = "whalerein/proto-schema-baseline/v1"
SNAPSHOT_SCHEMA = "whalerein/proto-fields/v1"


def _run(argv: list[str], stdin: bytes | None = None, cwd: Path | None = None) -> bytes:
    completed = subprocess.run(
        argv,
        input=stdin,
        cwd=str(cwd) if cwd else None,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"{' '.join(argv)} failed ({completed.returncode}): "
            + completed.stderr.decode("utf-8", "replace")[-1200:]
        )
    return completed.stdout


def descriptor_text(root: Path) -> str:
    with tempfile.TemporaryDirectory(prefix="whalerein-proto-") as tmp:
        descriptor = Path(tmp) / "descriptor.bin"
        _run(
            [
                "protoc",
                f"--proto_path={root / 'proto/v1'}",
                "--include_imports",
                f"--descriptor_set_out={descriptor}",
                str(root / PROTO_REL),
            ],
            cwd=root,
        )
        raw = _run(
            [
                "protoc",
                "--decode=google.protobuf.FileDescriptorSet",
                "google/protobuf/descriptor.proto",
            ],
            stdin=descriptor.read_bytes(),
            cwd=root,
        )
    return raw.decode("utf-8", "replace")


def _value(text: str) -> str:
    text = text.strip()
    if text.startswith('"') and text.endswith('"'):
        return text[1:-1]
    return text


def _parse_block(lines: list[str], index: int = 0) -> tuple[dict, int]:
    """Parse protoc's text format into ``{key: [value, ...]}``."""
    block: dict[str, list] = {}
    while index < len(lines):
        line = lines[index].strip()
        index += 1
        if not line:
            continue
        if line == "}":
            return block, index
        if line.endswith("{"):
            key = line[:-1].strip()
            value, index = _parse_block(lines, index)
            block.setdefault(key, []).append(value)
            continue
        key, _, value = line.partition(":")
        block.setdefault(key.strip(), []).append(_value(value))
    return block, index


def _single(block: dict, key: str, default=None):
    values = block.get(key, [])
    return values[0] if values else default


def parse_descriptor(text: str) -> dict:
    parsed, _ = _parse_block(text.splitlines())
    file_block = _single(parsed, "file", {})
    messages: dict[str, dict] = {}
    enums: dict[str, dict] = {}
    for message in file_block.get("message_type", []):
        name = _single(message, "name", "")
        oneof_names = [
            _single(oneof, "name", "") for oneof in message.get("oneof_decl", [])
        ]
        fields = []
        for field in message.get("field", []):
            number = int(_single(field, "number", "0"))
            optional = _single(field, "proto3_optional") == "true"
            oneof_index = _single(field, "oneof_index")
            oneof = None
            if oneof_index is not None and not optional:
                index = int(oneof_index)
                if index >= len(oneof_names):
                    raise RuntimeError(f"{name}.{_single(field, 'name')} has invalid oneof_index")
                oneof = oneof_names[index]
            entry = {
                "name": _single(field, "name", ""),
                "number": number,
                "type": _single(field, "type", ""),
                "json_name": _single(field, "json_name", ""),
                "repeated": _single(field, "label", "") == "LABEL_REPEATED",
            }
            if "type_name" in field:
                entry["type_name"] = _single(field, "type_name", "").lstrip(".")
            if oneof is not None:
                entry["oneof"] = oneof
            if optional:
                entry["optional"] = True
            fields.append(entry)
        messages[name] = {"fields": sorted(fields, key=lambda item: item["number"])}
    for enum in file_block.get("enum_type", []):
        name = _single(enum, "name", "")
        values = [
            {
                "name": _single(value, "name", ""),
                "number": int(_single(value, "number", "0")),
            }
            for value in enum.get("value", [])
        ]
        enums[name] = sorted(values, key=lambda item: item["number"])
    return {"messages": messages, "enums": enums}


def build(root: Path) -> dict:
    schema = parse_descriptor(descriptor_text(root))
    return {
        "schema": BASELINE_SCHEMA,
        "file": PROTO_REL,
        "messages": schema["messages"],
        "enums": schema["enums"],
    }


def compatibility_violations(baseline: dict, current: dict) -> list[str]:
    """Every breaking change from ``baseline`` to ``current``.

    Additions are compatible. Removals, renumbering, type changes, JSON-name
    changes and oneof membership changes are breaking.
    """
    violations: list[str] = []
    for message_name, baseline_message in sorted(baseline["messages"].items()):
        current_message = current["messages"].get(message_name)
        if current_message is None:
            violations.append(f"removed message {message_name}")
            continue
        by_number = {field["number"]: field for field in current_message["fields"]}
        by_name = {field["name"]: field for field in current_message["fields"]}
        for field in baseline_message["fields"]:
            label = f"{message_name}.{field['name']}"
            current_field = by_number.get(field["number"])
            if current_field is None:
                if field["name"] in by_name:
                    violations.append(
                        f"renumbered field {label} from {field['number']} to "
                        f"{by_name[field['name']]['number']}"
                    )
                else:
                    violations.append(f"removed field {label} (tag {field['number']})")
                continue
            if current_field["name"] != field["name"]:
                violations.append(
                    f"tag {field['number']} of {message_name} was reused from "
                    f"{field['name']} to {current_field['name']}"
                )
            for key in ("type", "type_name", "repeated", "json_name"):
                if current_field.get(key) != field.get(key):
                    violations.append(
                        f"changed {key} of {label}: {field.get(key)!r} -> "
                        f"{current_field.get(key)!r}"
                    )
            if current_field.get("oneof") != field.get("oneof"):
                violations.append(
                    f"changed oneof membership of {label}: "
                    f"{field.get('oneof')!r} -> {current_field.get('oneof')!r}"
                )
    for enum_name, baseline_values in sorted(baseline["enums"].items()):
        current_values = current["enums"].get(enum_name)
        if current_values is None:
            violations.append(f"removed enum {enum_name}")
            continue
        current_by_number = {value["number"]: value for value in current_values}
        current_by_name = {value["name"]: value for value in current_values}
        for value in baseline_values:
            label = f"{enum_name}.{value['name']}"
            current_value = current_by_number.get(value["number"])
            if current_value is None:
                if value["name"] in current_by_name:
                    violations.append(
                        f"renumbered enum value {label} from {value['number']} to "
                        f"{current_by_name[value['name']]['number']}"
                    )
                else:
                    violations.append(f"removed enum value {label} ({value['number']})")
                continue
            if current_value["name"] != value["name"]:
                violations.append(
                    f"enum number {value['number']} of {enum_name} was reused from "
                    f"{value['name']} to {current_value['name']}"
                )
    return violations


def _load(path: Path, schema: str) -> dict:
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema") != schema:
        raise SystemExit(f"{path} has an unexpected schema")
    return data


def check_baseline(root: Path) -> dict:
    path = root / BASELINE_REL
    if not path.is_file():
        raise SystemExit(f"missing compatibility baseline: {path}")
    baseline = _load(path, BASELINE_SCHEMA)
    current = build(root)
    violations = compatibility_violations(baseline, current)
    if violations:
        detail = "\n".join(f"  - {violation}" for violation in violations)
        raise SystemExit(
            "protobuf compatibility failed against "
            f"{BASELINE_REL}:\n{detail}\n"
            "Additive schema changes are compatible. For an intentional breaking "
            "change, run --update-baseline --rationale TEXT after bumping the "
            "protocol version."
        )
    return current


def check_snapshot(root: Path, current: dict) -> None:
    path = root / FIXTURE_REL
    if not path.is_file():
        raise SystemExit(f"missing golden fixture: {path}")
    expected = json.loads(path.read_text(encoding="utf-8"))
    if expected != current:
        diff = difflib.unified_diff(
            json.dumps(expected, indent=2, sort_keys=True).splitlines(),
            json.dumps(current, indent=2, sort_keys=True).splitlines(),
            fromfile="snapshot",
            tofile="descriptor",
            lineterm="",
        )
        raise SystemExit(
            "protobuf field snapshot failed:\n" + "\n".join(list(diff)[:200])
        )


def _write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def write_snapshot(root: Path) -> tuple[Path, Path]:
    """Refresh the exact snapshot and absorb additive baseline growth.

    A breaking change fails here, so ``--write`` cannot bless a removal.
    """
    current = check_baseline(root)
    snapshot = root / FIXTURE_REL
    _write_json(snapshot, current)
    baseline = root / BASELINE_REL
    _write_json(baseline, current)
    return snapshot, baseline


def update_baseline(root: Path, rationale: str) -> Path:
    path = root / BASELINE_REL
    baseline = _load(path, BASELINE_SCHEMA)
    current = build(root)
    violations = compatibility_violations(baseline, current)
    if violations and not rationale.strip():
        detail = "\n".join(f"  - {violation}" for violation in violations)
        raise SystemExit(
            "refusing to bless a breaking schema change without --rationale:\n" + detail
        )
    _write_json(path, current)
    if violations:
        changelog = root / CHANGELOG_REL
        changelog.parent.mkdir(parents=True, exist_ok=True)
        with changelog.open("a", encoding="utf-8") as handle:
            if changelog.stat().st_size == 0:
                handle.write("# Protocol compatibility baseline changes\n\n")
            handle.write("## Breaking change accepted\n\n")
            handle.write(f"Rationale: {rationale.strip()}\n\n")
            for violation in violations:
                handle.write(f"- {violation}\n")
            handle.write("\n")
    return path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[4]
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--update-baseline", action="store_true")
    parser.add_argument("--rationale", default="")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    if args.write:
        snapshot, baseline = write_snapshot(root)
        print(f"wrote {snapshot} and {baseline}")
        return 0
    if args.update_baseline:
        path = update_baseline(root, args.rationale)
        print(f"wrote {path}")
        return 0
    current = check_baseline(root)
    check_snapshot(root, current)
    print(
        "protobuf compatibility passed "
        f"({len(current['messages'])} messages, {len(current['enums'])} enums)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
