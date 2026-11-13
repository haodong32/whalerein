"""Fingerprint scheduled integration inputs and record verified full-suite success."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path


def fingerprint(root: Path, revision: str) -> str:
    tree = subprocess.run(
        ["git", "ls-tree", "-r", "-z", "--full-tree", revision],
        cwd=root,
        check=True,
        capture_output=True,
    ).stdout
    records = []
    for record in tree.split(b"\0"):
        if not record:
            continue
        path = record.split(b"\t", 1)[1].decode()
        if path.endswith(".md") or path.startswith(("docs/", ".local-docs/")):
            continue
        records.append(record)
    return hashlib.sha256(b"\0".join(records)).hexdigest()


def record_success(report: Path, output: Path, revision: str, key: str) -> None:
    document = json.loads(report.read_text())
    configuration = document["configuration"]
    scopes = document["scopes"]
    if (
        configuration["suite"] != "integration"
        or configuration.get("group")
        or configuration.get("only")
        or configuration.get("list_only")
        or document["completeness"] != "complete"
        or not scopes
    ):
        raise ValueError("Only complete full integration execution can establish a baseline")
    for scope in scopes:
        if (
            scope["exit_code"] != 0
            or scope.get("errors")
            or scope["completeness"] != "complete"
            or not scope["tests"]
            or any(case["status"] != "passed" for case in scope["tests"])
        ):
            raise ValueError("Integration execution or cleanup did not pass completely")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps({"revision": revision, "fingerprint": key, "suite": "integration"})
    )


def cached_revision(receipt: Path, key: str) -> str:
    try:
        data = json.loads(receipt.read_text())
        if (
            data["suite"] == "integration"
            and data["fingerprint"] == key
            and re.fullmatch(r"[0-9a-f]{40}", data["revision"])
        ):
            return data["revision"]
    except (OSError, ValueError, KeyError, TypeError):
        pass
    return ""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("key", "record", "cached"))
    parser.add_argument("--revision", required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    key = fingerprint(Path(__file__).resolve().parents[4], args.revision)
    if args.operation == "key":
        print(key)
    elif args.operation == "cached":
        print(cached_revision(args.output, key))
    else:
        record_success(args.report, args.output, args.revision, key)


if __name__ == "__main__":
    main()
