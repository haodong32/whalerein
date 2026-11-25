#!/usr/bin/env bash
set -euo pipefail

ios_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
version=2.48.0
checksum=620c9f0dc10849f94728ab5df52d225ebab8187439dd6fee1925e59e80dff892
cache_dir="$HOME/Library/Caches/whalerein/xcodegen"
install_dir="$cache_dir/$version"
xcodegen="$install_dir/bin/xcodegen"

if [[ ! -x "$xcodegen" ]]; then
  mkdir -p "$cache_dir"
  staging="$(mktemp -d "$cache_dir/install.XXXXXXXX")"
  trap 'rm -rf -- "$staging"' EXIT
  curl -fsSL --retry 3 "https://github.com/yonaskolb/XcodeGen/releases/download/$version/xcodegen.zip" -o "$staging/xcodegen.zip"
  printf '%s  %s\n' "$checksum" "$staging/xcodegen.zip" | shasum -a 256 -c -
  unzip -q "$staging/xcodegen.zip" -d "$staging"
  # Publish the complete installation, allowing another invocation to win the race.
  python3 - "$staging/xcodegen" "$install_dir" <<'PY'
import errno
import os
import sys

try:
    os.rename(sys.argv[1], sys.argv[2])
except OSError as error:
    if error.errno not in (errno.EEXIST, errno.ENOTEMPTY):
        raise
PY
fi

"$xcodegen" generate --spec "$ios_dir/project.yml" --use-cache
