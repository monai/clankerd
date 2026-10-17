#!/usr/bin/env bash
# Build the pinned dependency through the shared isolated cache.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
exec python3 "$root/scripts/dependency-build.py" vmnet-helper "${1:?Usage: build-vmnet-helper.sh OWNED_OUTPUT_DIR}"
