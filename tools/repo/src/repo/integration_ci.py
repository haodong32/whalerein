"""Fingerprint scheduled integration inputs and record verified full-suite success."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import tempfile
from pathlib import Path

from .reports.model import atomic_write
from .sources import SourceSelection, capture_sources


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


def record_success(
    report: Path,
    output: Path,
    revision: str,
    key: str,
    expected_source: str,
) -> None:
    document = json.loads(report.read_text())
    configuration = document["configuration"]
    scopes = document["scopes"]
    verification = document.get("verification", {})
    source = verification.get("sources", {}).get("public", {})
    artifacts = verification.get("artifacts", {})
    identities = verification.get("identities", {})
    collected = verification.get("collected_cases")
    observed = [case.get("id") for scope in scopes for case in scope["tests"]]
    if (
        verification.get("purpose") != "ci"
        or verification.get("suite") != "integration"
        or verification.get("exit_code") != 0
        or verification.get("case_filter") is not None
        or verification.get("cleanup") != {"status": "complete", "errors": []}
        or source.get("revision") != revision
        or source.get("mode") != "commit"
        or source.get("fingerprint") != expected_source
        or artifacts.get("products", {}).get("source") != source["fingerprint"]
        or not identities
        or any(
            not item.get("expected") or item.get("expected") != item.get("actual")
            for item in identities.values()
        )
        or not collected
        or len(set(collected)) != len(collected)
        or any(not case for case in observed)
        or sorted(observed) != sorted(collected)
    ):
        raise ValueError("Integration report does not prove the expected inputs and scope")
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
    atomic_write(
        output,
        json.dumps(
            {
                "revision": revision,
                "fingerprint": key,
                "suite": "integration",
                "report_digest": hashlib.sha256(report.read_bytes()).hexdigest(),
            }
        ),
    )


def cached_revision(receipt: Path, key: str) -> str:
    try:
        data = json.loads(receipt.read_text())
        if (
            data["suite"] == "integration"
            and data["fingerprint"] == key
            and re.fullmatch(r"[0-9a-f]{40}", data["revision"])
            and re.fullmatch(r"[0-9a-f]{64}", data["report_digest"])
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
    root = Path(__file__).resolve().parents[4]
    key = fingerprint(root, args.revision)
    if args.operation == "key":
        print(key)
    elif args.operation == "cached":
        print(cached_revision(args.output, key))
    else:
        with tempfile.TemporaryDirectory(prefix="whalerein-ci-inputs-") as temporary:
            expected = capture_sources(
                (SourceSelection("public", root, revision=args.revision),),
                Path(temporary) / "sources",
            )["public"]
        record_success(args.report, args.output, expected.revision, key, expected.fingerprint)


if __name__ == "__main__":
    main()
