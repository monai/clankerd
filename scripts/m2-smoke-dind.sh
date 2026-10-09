#!/usr/bin/env bash
# Runs the smoke test with the prepared DinD image from the existing local registry.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
port=${CLANKERD_DIND_REGISTRY_PORT:-5050}
registry=127.0.0.1:$port
export IMAGE=${IMAGE:-$registry/clankerd-dind:smoke}
export CLANKERD_INSECURE_REGISTRIES="${CLANKERD_INSECURE_REGISTRIES:+$CLANKERD_INSECURE_REGISTRIES,}$registry"
printf 'Smoke-test image: %s\n' "$IMAGE"
exec "$root/scripts/m2-smoke-test.sh"
