#!/usr/bin/env python3
"""Append the WhaleRein entry to a DSH profile patch document.

The generated DSH profile starts as a comment header followed by an empty
top-level array (`[]`). Appending a list item after that sentinel produces
invalid YAML, so an empty document is rewritten as comments plus the entry.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path


def is_empty_document(text: str) -> bool:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    content = [line for line in lines if not line.startswith("#")]
    return content in ([], ["[]"])


def render(text: str, host_id: str, state_dir: str) -> str:
    lines = text.splitlines()
    if is_empty_document(text):
        lines = [line for line in lines if line.strip() != "[]"]
    block = (
        "- id: whalerein-dsh\n"
        "  name: whalerein-dsh\n"
        "  config:\n"
        "    server: 127.0.0.1\n"
        f"    host_id: {json.dumps(host_id)}\n"
        "    host_token: !!js process.env.WHALEREIN_HOST_TOKEN\n"
        "    admin_token: !!js process.env.WHALEREIN_ADMIN_TOKEN ?? ''\n"
        f"    state_dir: {json.dumps(state_dir)}\n"
        "    adapter: real\n"
        "    allow_insecure_mqtt: true\n"
        "    log_level: debug\n"
    )
    prefix = "\n".join(lines).rstrip()
    return (prefix + "\n" if prefix.strip() else "") + block


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print("usage: profile-patch.py <cordis.patch.yml> <host-id> <state-dir>", file=sys.stderr)
        return 2
    path, host_id, state_dir = (Path(argv[1]), argv[2], argv[3])
    text = path.read_text(encoding="utf-8") if path.exists() else ""
    if "id: whalerein-dsh" in text:
        return 0
    path.write_text(render(text, host_id, state_dir), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
