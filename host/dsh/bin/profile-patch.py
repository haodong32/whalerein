#!/usr/bin/env python3
"""Seed the DSH profile or update explicitly requested connection scalars.

Existing custom settings, secrets, comments and sibling entries are preserved.
The helper never evaluates YAML tags or reads a shared TOML configuration.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
from pathlib import Path

ENTRY = re.compile(r"^- id: ['\"]?whalerein-dsh['\"]?\s*$")


def update(text: str, state: str, service: str | None, cloud: str | None) -> str:
    lines = text.splitlines()
    start = next((i for i, line in enumerate(lines) if ENTRY.fullmatch(line)), None)
    if start is None:
        if all(not line.strip() or line.lstrip().startswith('#') or line.strip() == '[]' for line in lines):
            lines = [line for line in lines if line.strip() != '[]']
        prefix = '\n'.join(lines).rstrip()
        block = ['- id: whalerein-dsh', '  name: whalerein-dsh', '  config:',
                 f'    service: {service or "whalerein_cloud"}',
                 f'    cloud_environment: {cloud or "production"}', '    adapter: real']
        if state:
            block.append(f'    state_dir: {json.dumps(state)}')
        return (prefix + '\n' if prefix else '') + '\n'.join(block) + '\n'
    if service is None and cloud is None:
        return text
    end = next((i for i in range(start + 1, len(lines)) if lines[i].startswith('- ')), len(lines))
    config = next((i for i in range(start + 1, end) if lines[i].strip() == 'config:'), None)
    if config is None:
        raise ValueError('the existing entry needs a block-style config before it can be edited')
    config_end = next((i for i in range(config + 1, end)
                       if lines[i].strip() and not lines[i].lstrip().startswith('#')
                       and len(lines[i]) - len(lines[i].lstrip()) <= 2), end)
    if any(re.match(r'^    (environment|config_file|connection_mode|profile_id):', line) for line in lines[config + 1:config_end]):
        raise ValueError('start the plugin once to migrate legacy selectors before an explicit override')
    changes = {}
    if service:
        changes['service'] = service
    if cloud:
        changes['cloud_environment'] = cloud
    block = lines[config + 1:config_end]
    for key, value in changes.items():
        pattern = re.compile(rf'^    {key}:')
        matches = [i for i, line in enumerate(block) if pattern.match(line)]
        if len(matches) > 1:
            raise ValueError('duplicate connection keys must be repaired before editing')
        if matches:
            block[matches[0]] = f'    {key}: {value}'
        else:
            block.append(f'    {key}: {value}')
    return '\n'.join(lines[:config + 1] + block + lines[config_end:]) + '\n'


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('path', type=Path)
    parser.add_argument('state_dir', nargs='?', default='')
    parser.add_argument('--service', choices=('whalerein_cloud', 'custom', 'standalone'))
    parser.add_argument('--cloud-environment', choices=('production', 'staging', 'local'))
    parser.add_argument('--print', dest='preview', action='store_true')
    args = parser.parse_args(argv)
    text = args.path.read_text(encoding='utf-8') if args.path.exists() else ''
    try:
        rendered = update(text, args.state_dir, args.service, args.cloud_environment)
    except ValueError as error:
        parser.error(str(error))
    if args.preview:
        sys.stdout.write('\n'.join(line for line in rendered.splitlines() if re.match(r'^    (service|cloud_environment|adapter|state_dir|state_root):', line)) + '\n')
    elif rendered != text:
        with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=args.path.parent, delete=False) as output:
            temporary = Path(output.name)
            output.write(rendered)
        try:
            temporary.replace(args.path)
        finally:
            temporary.unlink(missing_ok=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
