#!/usr/bin/env bash
# Prepares a Docker build context for the official DinD image plus clankerd setup.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/build/dind-smoke-image
mkdir -p "$out"

cp "$root/images/slim/daemon.json" "$out/daemon.json"
cp "$root/images/slim/clankerd-dockerd" "$out/clankerd-dockerd"
cp "$root/images/dind/Dockerfile" "$root/images/dind/clankerd-dind-entrypoint" "$out/"

printf 'Prepared DinD build context: %s\n' "$out"
