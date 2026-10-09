#!/usr/bin/env bash
# Prepares a Docker build context for the official DinD image plus clankerd setup.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/build/dind-smoke-image
mkdir -p "$out"

cp "$root/contrib/image/slim/daemon.json" "$out/daemon.json"
cp "$root/contrib/image/slim/clankerd-dockerd" "$out/clankerd-dockerd"

cat > "$out/Dockerfile" <<'EOF'
FROM docker:29-dind
RUN apk add --no-cache nftables \
 && docker compose version \
 && nft --version
ENV DOCKER_HOST=unix:///var/run/docker.sock
ENV DOCKER_TLS_CERTDIR=""
COPY daemon.json /etc/docker/daemon.json
COPY clankerd-dockerd clankerd-dind-entrypoint /usr/local/bin/
RUN chmod 0755 /usr/local/bin/clankerd-dockerd /usr/local/bin/clankerd-dind-entrypoint
ENTRYPOINT ["clankerd-dind-entrypoint"]
CMD ["sleep", "infinity"]
EOF

cat > "$out/clankerd-dind-entrypoint" <<'EOF'
#!/bin/sh
set -eu
if [ -e /.clankerdenv ]; then
    clankerd-dockerd
    exec "$@"
fi
exec /usr/local/bin/dockerd-entrypoint.sh "$@"
EOF

printf 'Prepared DinD build context: %s\n' "$out"
