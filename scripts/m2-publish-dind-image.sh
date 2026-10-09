#!/usr/bin/env bash
# Makes the locally built image available to vmctl through a loopback registry.
set -euo pipefail

image=${CLANKERD_DIND_IMAGE:-clankerd-dind:smoke}
port=${CLANKERD_DIND_REGISTRY_PORT:-5050}
case "$port" in '' | *[!0-9]*) echo "Registry port must be numeric." >&2; exit 1 ;; esac
[ "$port" -ge 1 ] && [ "$port" -le 65535 ] || { echo "Invalid registry port." >&2; exit 1; }
registry=127.0.0.1:$port
remote=$registry/clankerd-dind:smoke

platform=$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$image")
[ "$platform" = linux/arm64 ] || { echo "Expected linux/arm64 image, got $platform." >&2; exit 1; }

if ! curl --fail --silent --show-error --max-time 5 "http://$registry/v2/" >/dev/null; then
    echo "Existing registry is not reachable at http://$registry." >&2
    exit 1
fi

docker tag "$image" "$remote"
docker push "$remote"
printf 'Image available to vmctl: %s\n' "$remote"
