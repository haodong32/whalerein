#!/usr/bin/env bash
set -euo pipefail

# This removes SDKs only from a disposable GitHub-hosted Linux VM. Integration
# compilers and browser dependencies come from the pinned Docker build stages.
if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_ENVIRONMENT:-}" != github-hosted || "${RUNNER_OS:-}" != Linux ]]; then
  printf '%s\n' 'Integration storage preparation requires a GitHub-hosted Linux runner.' >&2
  exit 1
fi

if [[ "$(uname -s)" != Linux ]]; then
  printf '%s\n' 'Integration storage preparation requires a native Linux kernel.' >&2
  exit 1
fi

minimum_mib=24576
free_mib() {
  df -Pm "${GITHUB_WORKSPACE:?}" | awk 'NR == 2 {print $4}'
}

printf 'Integration storage before cleanup: %s MiB free\n' "$(free_mib)"
# No test container exists yet. Keep active images and all volumes untouched.
docker image prune --all --force
for candidate in /usr/local/lib/android /usr/share/dotnet /usr/local/.ghcup /opt/ghc /opt/hostedtoolcache/CodeQL /usr/local/share/powershell /usr/share/swift; do
  if (( $(free_mib) >= minimum_mib )); then
    break
  fi
  if [[ -d "$candidate" ]]; then
    printf 'Removing unused hosted-runner SDK: %s\n' "$candidate"
    sudo -n rm -rf -- "$candidate"
  fi
done
remaining_mib=$(free_mib)
printf 'Integration storage after cleanup: %s MiB free\n' "$remaining_mib"
if (( remaining_mib < minimum_mib )); then
  printf 'Integration image preparation requires at least %s MiB free.\n' "$minimum_mib" >&2
  exit 1
fi
