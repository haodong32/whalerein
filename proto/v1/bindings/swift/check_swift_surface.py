#!/usr/bin/env python3
"""Swift-visible drift check for the mobile surface.

iOS does not import generated Swift protobuf types; it imports the UniFFI
`WhaleReinMobile` module, so the meaningful Swift contract is the flat
`MobileError` surface and its mapping from every wire `ErrorCode`.

The check fails when:

* a wire `ErrorCode` in ``proto/v1/error_codes.json`` has no recorded Swift
  classification in ``proto/v1/bindings/swift/ios-surface.json``;
* a recorded Swift case is absent from the hand-written extension file, or no
  longer returns the case's stable code;
* the generated Swift module exists but a required symbol is missing.

A recorded ``follow-up`` status means the current flat enum cannot express the
wire action yet (for example ``protocol_version_incompatible`` must become an
upgrade prompt, not a retryable ``unavailable``). It is reported, not silently
ignored, and a new code cannot join it without an explicit snapshot edit.

Usage:
  check_swift_surface.py [--root DIR]
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ERROR_CODES_REL = "proto/v1/error_codes.json"
SURFACE_REL = "proto/v1/bindings/swift/ios-surface.json"
SCHEMA = "whalerein/ios-surface/v1"


def load(root: Path) -> tuple[dict, dict]:
    codes = json.loads((root / ERROR_CODES_REL).read_text(encoding="utf-8"))
    surface = json.loads((root / SURFACE_REL).read_text(encoding="utf-8"))
    if surface.get("schema") != SCHEMA:
        raise SystemExit(f"{SURFACE_REL} has an unexpected schema")
    return codes, surface


def check(root: Path) -> dict:
    codes, surface = load(root)
    entries = surface["errorCodes"]
    problems: list[str] = []
    follow_ups: list[str] = []
    names = [entry["name"] for entry in codes["codes"]]
    for name in names:
        entry = entries.get(name)
        if entry is None:
            problems.append(f"wire code {name} has no Swift classification")
            continue
        if entry.get("status") not in ("mapped", "follow-up"):
            problems.append(f"{name} has invalid status {entry.get('status')!r}")
        if entry.get("status") == "follow-up":
            follow_ups.append(name)
    for name in entries:
        if name not in names:
            problems.append(f"stale Swift classification for removed wire code {name}")

    swift_file = root / surface["swiftErrorFile"]
    if not swift_file.is_file():
        problems.append(f"missing Swift error extension {surface['swiftErrorFile']}")
    else:
        text = swift_file.read_text(encoding="utf-8")
        for name, entry in entries.items():
            swift_case = entry["swiftCase"]
            if not re.search(rf"case \.{re.escape(swift_case)}\b", text):
                problems.append(f"{name} maps to missing Swift case .{swift_case}")
        for swift_case in {entry["swiftCase"] for entry in entries.values()}:
            if not re.search(rf"case \.{re.escape(swift_case)}\b.*?return \"", text, re.S):
                problems.append(f"Swift case .{swift_case} does not return a stable code")

    generated = root / surface["generatedSwiftFile"]
    generated_present = generated.is_file()
    if generated_present:
        module = generated.read_text(encoding="utf-8", errors="replace")
        for symbol in surface["requiredGeneratedSymbols"]:
            if symbol not in module:
                problems.append(f"generated Swift module is missing {symbol}")
    if problems:
        raise SystemExit("Swift surface drift:\n" + "\n".join(f"  - {item}" for item in problems))
    return {
        "codes": len(entries),
        "followUps": sorted(follow_ups),
        "generatedSwiftChecked": generated_present,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[4])
    args = parser.parse_args(argv)
    result = check(args.root.resolve())
    generated = "checked" if result["generatedSwiftChecked"] else "not built; checked in-tree surface only"
    print(
        f"Swift surface drift check passed ({result['codes']} wire codes, "
        f"{len(result['followUps'])} recorded follow-ups, generated module {generated})"
    )
    if result["followUps"]:
        print("recorded follow-ups: " + ", ".join(result["followUps"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
