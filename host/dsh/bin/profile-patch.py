#!/usr/bin/env python3
"""Append the WhaleRein entry to a DSH profile patch document.

The generated DSH profile starts as a comment header followed by an empty
top-level array (`[]`). Appending a list item after that sentinel produces
invalid YAML, so an empty document is rewritten as comments plus the entry.
"""

from __future__ import annotations

import os
import json
import sys
from pathlib import Path


def is_empty_document(text: str) -> bool:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    content = [line for line in lines if not line.startswith("#")]
    return content in ([], ["[]"])


def render(text: str, state_dir: str) -> str:
    """Seed one managed entry with no endpoint or trust material.

    The per-device TOML path stays a Host/DSH setting, not a generated Cloud
    descriptor: an empty config_file means the Host default
    (~/.whalerein/config.toml) or WHALEREIN_CONFIG when the operator sets it.
    """
    lines = text.splitlines()
    if is_empty_document(text):
        lines = [line for line in lines if line.strip() != "[]"]
    block = (
        "- id: whalerein-dsh\n"
        "  name: whalerein-dsh\n"
        "  config:\n"
        "    connection_mode: managed\n"
        "    adapter: real\n"
        "    log_level: debug\n"
    )
    if state_dir:
        block += f"    state_dir: {json.dumps(state_dir)}\n"
    prefix = "\n".join(lines).rstrip()
    return (prefix + "\n" if prefix.strip() else "") + block

def main(argv: list[str]) -> int:
    if len(argv) not in (2, 3):
        print("usage: profile-patch.py <cordis.patch.yml> [state-dir]", file=sys.stderr)
        return 2
    path = Path(argv[1])
    state_dir = argv[2] if len(argv) == 3 else ""
    text = path.read_text(encoding="utf-8") if path.exists() else ""
    if "id: whalerein-dsh" in text:
        return 0
    path.write_text(render(text, state_dir), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
