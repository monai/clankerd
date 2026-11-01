#!/usr/bin/env bash
set -euo pipefail

context=$(cd "$(dirname "$0")" && pwd)
image=${1:-clankerd-slim:local}

if [ "$#" -gt 1 ]; then
    echo "Usage: $0 [IMAGE_TAG]" >&2
    exit 1
fi
if ! command -v docker >/dev/null 2>&1; then
    echo "Docker CLI is unavailable on this machine." >&2
    exit 1
fi
if ! docker info >/dev/null 2>&1; then
    echo "Cannot reach the current Docker daemon; check that it is running." >&2
    exit 1
fi
if ! docker buildx version >/dev/null 2>&1; then
    echo "The Docker Buildx plugin is unavailable." >&2
    exit 1
fi

docker buildx build --platform linux/arm64 --load --tag "$image" "$context"
docker image inspect --format '{{.Os}}/{{.Architecture}} {{.Id}}' "$image"
printf 'Built local image: %s\n' "$image"
