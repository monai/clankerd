#!/usr/bin/env bash
# The prototype's end-to-end smoke test (ticket 10): Docker inside a clankerd
# machine, driven only through the vmctl CLI. Run on the Apple Silicon Mac
# after `make rust`, with the slim image changes applied
# (docs/docker-in-machine.md). Exits non-zero on the first failed check, and
# always removes its machines and volume. Not run in CI (GitHub's macOS
# runners cannot run VMs); a self-hosted runner can call it as is.
#
#   scripts/m2-smoke-test.sh
#   IMAGE=ghcr.io/monai/clankers:slim OUT=build/rust scripts/m2-smoke-test.sh
#
# Environment (all optional):
#   IMAGE               image under test (default ghcr.io/monai/clankers:slim)
#   ALT_IMAGE           a second image for the "change the image, keep the
#                       volume" step (default debian:bookworm-slim)
#   OUT                 build output directory (default build/rust)
#   VMCTL               use this vmctl as is; skips the macOS and build checks
#   SMOKE_HOST_PORT     host port published to nginx (default 8080)
#   SMOKE_HOSTGW_PORT   host loopback port the guest must reach (default 18081)
#   SMOKE_TIMEOUT       seconds to wait for dockerd and for services (default 120)
#
# What it proves, in order: boot with a volume; interactive exec; dockerd in
# nftables mode on the stock kernel; `docker run -p 8080:80 nginx` reachable
# from the Mac; a compose project with a user-defined network; containers reach
# the internet; a host loopback port reachable from the guest; stop/start with
# the volume (and Docker's data on it) intact; a changed image on the same
# volume; a second machine from the cached image.
set -Eeuo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
image=${IMAGE:-ghcr.io/monai/clankers:slim}
alt_image=${ALT_IMAGE:-debian:bookworm-slim}
host_port=${SMOKE_HOST_PORT:-8080}
gw_port=${SMOKE_HOSTGW_PORT:-18081}
timeout_s=${SMOKE_TIMEOUT:-120}
vol=smoke-data

if [ -n "${VMCTL:-}" ]; then
  vmctl=$VMCTL
else
  vmctl=$out/darwin-arm64/vmctl
  export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
  export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
fi
state=$(mktemp -d "${TMPDIR:-/tmp}/clankerd-smoke.XXXXXX")
export CLANKERD_STATE_DIR=$state/state
export CLANKERD_RUNTIME_DIR=$state/run
http_pid=

cleanup() {
  [ -z "$http_pid" ] || kill "$http_pid" >/dev/null 2>&1 || true
  for m in smoke smoke-alt smoke2; do
    "$vmctl" rm -f -v "$m" >/dev/null 2>&1 || true
  done
  "$vmctl" volume rm "$vol" >/dev/null 2>&1 || true
  rm -rf "$state"
}
trap cleanup EXIT

trap 'fail "command failed (line $LINENO): $BASH_COMMAND"' ERR

step() { printf '\n== %s\n' "$*"; }
fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

# Runs a shell snippet inside the machine; used for every in-guest assertion.
guest() { "$vmctl" exec smoke sh -c "$1"; }

# Retries a command until $timeout_s passes.
retry() { # description, command...
  local what=$1 i=0
  shift
  until "$@" >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -le "$timeout_s" ] || fail "timed out waiting for $what"
    sleep 1
  done
}

host_get() { curl -fsS --max-time 5 "http://127.0.0.1:$host_port/"; }
nginx_up() { host_get | grep -q 'Welcome to nginx'; }
dockerd_up() { guest 'docker info'; }
exec_up() { "$vmctl" exec "$1" true; }

diagnose() {
  {
    echo "--- dockerd log"
    "$vmctl" exec smoke cat /var/log/dockerd.log 2>&1 | tail -n 40
    echo "--- machine console"
    "$vmctl" logs smoke 2>&1 | tail -n 40
  } >&2 || true
}

step "prerequisites"
if [ -z "${VMCTL:-}" ]; then
  [ "$(uname -sm)" = "Darwin arm64" ] || fail "run this on the Apple Silicon Mac"
  for f in "$vmctl" "$CLANKERD_VMSPAWN" "$CLANKERD_GUESTD" "$out/linux-arm64/mke2fs"; do
    [ -x "$f" ] || fail "$f missing: run make rust"
  done
fi
command -v curl >/dev/null || fail "curl is required on the host"
command -v python3 >/dev/null || fail "python3 is required on the host"
echo ok

step "pull $image and $alt_image"
"$vmctl" pull "$image"
"$vmctl" pull "$alt_image"

step "host loopback service on 127.0.0.1:$gw_port"
mkdir -p "$state/www"
echo from-host > "$state/www/hello"
python3 -m http.server "$gw_port" --bind 127.0.0.1 --directory "$state/www" >/dev/null 2>&1 &
http_pid=$!

step "boot with a volume, a published port and a host-gateway port"
"$vmctl" run -d --name smoke -v "$vol:/storage" --volume-size 20G \
  -p "$host_port:80" --host-gateway-port "$gw_port" "$image" sleep infinity
retry "the machine to accept exec" exec_up smoke

step "interactive exec: stdin reaches the guest and output returns"
reply=$(printf 'echo interactive-ok\nexit\n' | "$vmctl" exec -i smoke sh)
case "$reply" in *interactive-ok*) echo ok ;; *) fail "interactive exec answered: $reply" ;; esac

step "dockerd runs in nftables mode on the stock kernel"
retry "dockerd" dockerd_up
if ! guest 'docker info 2>&1 | grep -qi nftables && nft list tables | grep -q docker'; then
  diagnose
  fail "dockerd is not in nftables mode (see docs/docker-in-machine.md, the nftables failure section). Record the dockerd log above, then evaluate the Kata kernel fallback (krun_set_kernel)."
fi
guest 'docker version --format "docker {{.Server.Version}}"'

step "docker run -p 8080:80 nginx is reachable from the Mac on 127.0.0.1:$host_port"
guest 'docker run -d --name web -p 8080:80 nginx'
retry "nginx on the Mac's published port" nginx_up
echo "fetched nginx from the Mac"

step "containers reach the internet (DNS and TCP)"
guest 'docker run --rm busybox wget -qO- http://example.com | grep -qi "example domain"' \
  || fail "a container cannot reach the internet"

step "compose project with a user-defined network"
guest 'mkdir -p /tmp/proj && cat > /tmp/proj/compose.yaml <<EOF2
services:
  web:
    image: nginx:alpine
    networks: [appnet]
  client:
    image: busybox
    command: sleep infinity
    networks: [appnet]
networks:
  appnet: {}
EOF2
docker compose -p proj -f /tmp/proj/compose.yaml up -d'
guest 'docker compose -p proj -f /tmp/proj/compose.yaml exec -T client wget -qO- http://web | grep -q "Welcome to nginx"' \
  || fail "compose services cannot reach each other by name"
guest 'docker network inspect proj_appnet >/dev/null' || fail "the user-defined network is missing"
guest 'docker compose -p proj -f /tmp/proj/compose.yaml down'

step "a host loopback port is reachable from the guest"
guest "wget -qO- http://127.0.0.1:$gw_port/hello | grep -q from-host" \
  || fail "the guest cannot reach the host's loopback port $gw_port"

step "stop and start keep the volume and Docker's data"
guest 'echo persisted > /storage/marker && sync'
"$vmctl" stop smoke
"$vmctl" start smoke
retry "the machine to accept exec" exec_up smoke
retry "dockerd after restart" dockerd_up
# shellcheck disable=SC2016 # expanded in the guest
guest '[ "$(cat /storage/marker)" = persisted ]' || fail "volume marker (persisted) lost across stop/start"
guest 'docker image inspect nginx >/dev/null' || fail "Docker's data-root did not persist (nginx image gone)"
guest 'docker start web >/dev/null'
retry "nginx on the published port after restart" nginx_up

step "a changed image keeps the volume"
"$vmctl" stop smoke
"$vmctl" rm smoke
"$vmctl" run -d --name smoke-alt -v "$vol:/storage" "$alt_image" sleep infinity
retry "the alternate machine to accept exec" exec_up smoke-alt
# shellcheck disable=SC2016 # expanded in the guest
"$vmctl" exec smoke-alt sh -c '[ "$(cat /storage/marker)" = persisted ]' \
  || fail "volume marker (persisted) lost when the image changed"
"$vmctl" rm -f smoke-alt

step "a second machine from the cached image is created almost instantly"
began=$(date +%s)
"$vmctl" create --name smoke2 "$image" true >/dev/null
took=$(($(date +%s) - began))
echo "create took ${took}s"
[ "$took" -le 10 ] || fail "second create took ${took}s (is the base disk cloned?)"

step "done"
echo "PASS: Docker inside a clankerd machine works end to end."
