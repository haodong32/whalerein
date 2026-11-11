#!/usr/bin/env python3
"""Protobuf field-compatibility check for proto/v1/whalerein.proto.

Uses protoc's descriptor set and its text form (stdlib parsing only) to record
every message field number/type/name and enum value. A stale golden file fails,
which surfaces deletions, renames, renumbering and type changes.

Usage:
  proto_compat.py --write [--root DIR]
  proto_compat.py --check [--root DIR]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

FIXTURE_REL = "tests/fixtures/proto/fields.json"
PROTO_REL = "proto/v1/whalerein.proto"
SCHEMA = "whalerein/proto-fields/v1"


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


def parse_descriptor(text: str) -> dict:
    messages: dict[str, dict] = {}
    enums: dict[str, dict] = {}
    current_message: dict | None = None
    current_enum: dict | None = None
    current_field: dict | None = None
    current_enum_value: dict | None = None

    for raw_line in text.splitlines():
        indent = len(raw_line) - len(raw_line.lstrip())
        line = raw_line.strip()
        if not line:
            continue
        if indent == 2 and line.startswith("message_type {"):
            current_message = {"name": "", "fields": []}
            current_enum = None
            current_field = None
            continue
        if indent == 2 and line.startswith("enum_type {"):
            current_enum = {"name": "", "values": []}
            current_message = None
            current_enum_value = None
            continue
        if indent == 2 and line == "}":
            if current_message is not None:
                messages[current_message["name"]] = {
                    "fields": sorted(
                        current_message["fields"], key=lambda item: item["number"]
                    )
                }
                current_message = None
            elif current_enum is not None:
                enums[current_enum["name"]] = {
                    "values": sorted(
                        current_enum["values"], key=lambda item: item["number"]
                    )
                }
                current_enum = None
            continue
        if current_message is not None:
            if indent == 4 and line == "name:":
                continue
            if indent == 4 and line.startswith("name:"):
                current_message["name"] = _value(line.split(":", 1)[1])
                continue
            if indent == 4 and line == "field {":
                current_field = {}
                continue
            if indent == 4 and line == "}" and current_field is not None:
                current_message["fields"].append(current_field)
                current_field = None
                continue
            if current_field is not None and indent == 6 and ":" in line:
                key, value = line.split(":", 1)
                current_field[key.strip()] = _value(value)
                continue
        if current_enum is not None:
            if indent == 4 and line.startswith("name:"):
                current_enum["name"] = _value(line.split(":", 1)[1])
                continue
            if indent == 4 and line == "value {":
                current_enum_value = {}
                continue
            if indent == 4 and line == "}" and current_enum_value is not None:
                current_enum["values"].append(current_enum_value)
                current_enum_value = None
                continue
            if current_enum_value is not None and indent == 6 and ":" in line:
                key, value = line.split(":", 1)
                value = _value(value)
                current_enum_value[key.strip()] = (
                    int(value) if key.strip() == "number" else value
                )
                continue

    for message in messages.values():
        for field in message["fields"]:
            field.pop("label", None) if field.get("label") == "LABEL_OPTIONAL" else None
            if "type_name" in field:
                field["type_name"] = field["type_name"].lstrip(".")
    return {"schema": SCHEMA, "file": PROTO_REL, "messages": messages, "enums": enums}


def build(root: Path) -> dict:
    text = descriptor_text(root)
    return parse_descriptor(text)


def write_fixture(root: Path) -> Path:
    fixture = build(root)
    path = root / FIXTURE_REL
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(fixture, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return path


def check_fixture(root: Path) -> tuple[int, int]:
    path = root / FIXTURE_REL
    if not path.is_file():
        raise SystemExit(f"missing golden fixture: {path}")
    expected = json.loads(path.read_text(encoding="utf-8"))
    actual = build(root)
    if actual != expected:
        import difflib

        diff = difflib.unified_diff(
            json.dumps(expected, indent=2, sort_keys=True).splitlines(),
            json.dumps(actual, indent=2, sort_keys=True).splitlines(),
            fromfile="golden",
            tofile="descriptor",
            lineterm="",
        )
        raise SystemExit(
            "protobuf field compatibility failed:\n" + "\n".join(list(diff)[:200])
        )
    return len(actual["messages"]), len(actual["enums"])


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[4]
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    if args.write:
        path = write_fixture(root)
        print(f"wrote {path}")
        return 0
    messages, enums = check_fixture(root)
    print(f"protobuf field compatibility passed ({messages} messages, {enums} enums)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
