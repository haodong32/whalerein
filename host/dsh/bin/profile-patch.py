#!/usr/bin/env python3
"""Write the WhaleRein entry into a DSH profile patch document.

The DSH profile patch is a YAML list. A fresh profile starts as comments plus
an empty top-level array (`[]`), so appending a block after that sentinel would
produce invalid YAML. This script handles three cases:

* an empty document: drop the `[]` sentinel and append the entry;
* an existing `whalerein-dsh` entry: leave it alone unless ``--force`` is given,
  in which case only that list item is replaced;
* ``--print``: render to stdout without touching the file (dry run).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ID_LINE = "- id: whalerein-dsh"


def is_empty_document(text: str) -> bool:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    content = [line for line in lines if not line.startswith("#")]
    return content in ([], ["[]"])


def render(mode: str, *, host_id: str, state_dir: str, state_root: str,
           developer_tools: bool, log_level: str) -> str:
    if mode == "managed":
        lines = [
            ID_LINE,
            "  name: whalerein-dsh",
            "  config:",
            "    connection_mode: managed",
            f"    developer_tools_enabled: {'true' if developer_tools else 'false'}",
            "    adapter: real",
            f"    log_level: {log_level}",
        ]
        if state_root:
            lines.append(f"    state_root: {json.dumps(state_root)}")
        elif state_dir:
            lines.append(f"    state_dir: {json.dumps(state_dir)}")
    else:
        lines = [
            ID_LINE,
            "  name: whalerein-dsh",
            "  config:",
            "    server: 127.0.0.1",
            f"    host_id: {json.dumps(host_id)}",
            "    host_token: !!js process.env.WHALEREIN_HOST_TOKEN",
            "    admin_token: !!js process.env.WHALEREIN_ADMIN_TOKEN ?? ''",
            f"    state_dir: {json.dumps(state_dir)}",
            "    adapter: real",
            "    allow_insecure_mqtt: true",
            f"    log_level: {log_level}",
        ]
    return "\n".join(lines) + "\n"


def replace_entry(text: str, block: str) -> str:
    """Replace the existing whalerein-dsh list item, keeping every other line."""
    lines = text.splitlines()
    start = next((i for i, line in enumerate(lines) if line.strip() == ID_LINE), None)
    if start is None:
        return append_entry(text, block)
    end = len(lines)
    for i in range(start + 1, len(lines)):
        # A new top-level list item starts every sibling entry.
        if lines[i].startswith("- "):
            end = i
            break
    prefix = lines[:start]
    suffix = lines[end:]
    rendered = prefix + block.splitlines() + suffix
    return "\n".join(rendered).rstrip() + "\n"


def append_entry(text: str, block: str) -> str:
    lines = text.splitlines()
    if is_empty_document(text):
        lines = [line for line in lines if line.strip() != "[]"]
    prefix = "\n".join(lines).rstrip()
    return (prefix + "\n" if prefix.strip() else "") + block


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(add_help=True)
    parser.add_argument("path", type=Path)
    # Preserve the managed <path> [state-dir] interface.
    parser.add_argument("legacy_state_dir", nargs="?", default="")
    parser.add_argument("--mode", choices=("managed", "standalone"), default="managed")
    parser.add_argument("--host-id", default=None)
    parser.add_argument("--state-dir", default="")
    parser.add_argument("--state-root", default="")
    parser.add_argument("--developer-tools", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--log-level", default="info")
    parser.add_argument("--force", action="store_true", help="replace an existing entry")
    parser.add_argument("--print", dest="print_only", action="store_true")
    args = parser.parse_args(argv)

    mode = args.mode
    host_id = args.host_id or "host-1"
    state_dir = args.state_dir or args.legacy_state_dir or ""
    text = args.path.read_text(encoding="utf-8") if args.path.exists() else ""
    block = render(mode, host_id=host_id, state_dir=state_dir,
                   state_root=args.state_root, developer_tools=args.developer_tools,
                   log_level=args.log_level)
    if args.print_only:
        if ID_LINE in text:
            sys.stdout.write(replace_entry(text, block) if args.force else text)
        else:
            sys.stdout.write(append_entry(text, block))
        return 0
    if ID_LINE in text:
        if not args.force:
            return 0
        updated = replace_entry(text, block)
    else:
        updated = append_entry(text, block)
    args.path.write_text(updated, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
