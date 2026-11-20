#!/usr/bin/env python3
"""Release-decision check for the grant-fencing wire metadata.

``proto/v1/whalerein.proto`` carries both durable Access revisions in
``RequestTransportMetadata``. Access captures them at submission and compares
both against the current durable grant row at ciphertext release. This checker
makes the documented matrix executable:

* a stored result from before a revoke is denied on first GET and on repeat GET;
* a revoke followed by a re-grant keeps the old result denied;
* a request admitted after the re-grant is released;
* a QUERY without a stamp fails closed;
* a COMMAND keeps metadata-only recovery and never releases the body without an
  exact stamp.

The vectors are wire metadata only. Access never inspects the encrypted body.

Usage:
  grant_fencing.py --check [--root DIR]
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

VECTORS_REL = "tests/fixtures/proto/grant_fencing.json"
SCHEMA = "whalerein/grant-fencing-vectors/v1"


class VectorError(Exception):
    pass


def _stamp(stored: object) -> tuple[int, int] | None:
    if stored is None:
        return None
    if not isinstance(stored, dict):
        raise VectorError("stored stamp must be an object or null")
    grant = stored.get("grantGeneration")
    revocation = stored.get("revocationGeneration")
    for value in (grant, revocation):
        if not isinstance(value, str) or not value.isdigit():
            raise VectorError("stamp revisions must be decimal strings")
    return int(grant), int(revocation)


def release(stored: object, current: dict) -> bool:
    stamp = _stamp(stored)
    if stamp is None or not current["active"]:
        return False
    return stamp == (int(current["grantGeneration"]), int(current["revocationGeneration"]))


def check(root: Path) -> int:
    path = root / VECTORS_REL
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema") != SCHEMA:
        raise VectorError(f"{VECTORS_REL}: schema must be {SCHEMA}")
    entries = data.get("vectors")
    if not isinstance(entries, list) or not entries:
        raise VectorError(f"{VECTORS_REL}: vectors must be a non-empty list")
    seen: set[str] = set()
    for entry in entries:
        identity = entry.get("id")
        if not isinstance(identity, str) or not identity or identity in seen:
            raise VectorError("every vector needs a unique id")
        seen.add(identity)
        kind = entry.get("kind")
        if kind not in {"query", "command"}:
            raise VectorError(f"{identity}: kind must be query or command")
        expect = entry.get("expect", {})
        if expect.get("body") not in {"release", "deny"}:
            raise VectorError(f"{identity}: expect.body must be release or deny")
        if expect.get("metadata") != "available":
            raise VectorError(f"{identity}: metadata must stay available")
        if expect["body"] == "deny" and expect.get("errorCode") != "forbidden":
            raise VectorError(f"{identity}: denied bodies use forbidden")
        if expect["body"] == "release" and "errorCode" in expect:
            raise VectorError(f"{identity}: released bodies carry no error code")
        actual = "release" if release(entry.get("stored"), entry["current"]) else "deny"
        if actual != expect["body"]:
            raise VectorError(f"{identity}: release decision {actual} != {expect['body']}")
    required = {
        "stored-before-revoke-first-get",
        "repeat-get-after-revoke",
        "revoke-then-regrant",
        "missing-stamp-query-fails-closed",
    }
    missing = required - seen
    if missing:
        raise VectorError("missing vectors: " + ", ".join(sorted(missing)))
    print(f"grant fencing vectors passed ({len(entries)} vectors)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[4]
    )
    parser.add_argument("--check", action="store_true", required=True)
    args = parser.parse_args(argv)
    try:
        return check(args.root.resolve())
    except (OSError, json.JSONDecodeError, KeyError, VectorError) as exc:
        print(f"grant fencing vectors failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
