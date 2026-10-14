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
#   SMOKE_TIMEOUT       seconds to wait for dockerd and for services (default 120)
#
# What it proves, in order: boot with a volume; interactive exec; dockerd in
# nftables mode on the stock kernel; `docker run -p 8080:80 nginx` reachable
# from the Mac; a compose project with a user-defined network; containers reach
# the internet; stop/start with
# the volume (and Docker's data on it) intact; a changed image on the same
# volume; a second machine from the cached image.
set -Eeuo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
image=${IMAGE:-ghcr.io/monai/clankers:slim}
alt_image=${ALT_IMAGE:-debian:bookworm-slim}
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

cleanup() {
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
    [ "$i" -le "$timeout_s" ] || { diagnose; fail "timed out waiting for $what"; }
    sleep 1
  done
}

host_get() {
  local guest_ip
  guest_ip=$(guest "ip -4 -o addr show dev eth0 | awk '{split(\$4,a,\"/\"); print a[1]}'")
  [ -n "$guest_ip" ] || return 1
  curl --noproxy '*' -fsS --max-time 5 "http://$guest_ip:8080/"
}
nginx_up() { host_get | grep -q 'Welcome to nginx'; }
dockerd_up() { guest 'docker info'; }
registry_dns_up() { guest 'timeout 5 getent ahosts registry-1.docker.io'; }
exec_up() { "$vmctl" exec "$1" true; }

diagnose() {
  {
    echo "--- guest resolver and routes"
    guest 'cat /etc/resolv.conf; ip address show dev eth0; ip route; ip -6 route' 2>&1
    echo "--- current network configuration"
    "$vmctl" inspect smoke 2>&1
    echo "--- published container and forwarding"
    guest 'docker ps -a; docker port web; docker inspect --format "running={{.State.Running}} error={{.State.Error}} networks={{json .NetworkSettings.Networks}}" web; docker logs --tail 15 web; cat /proc/sys/net/ipv4/ip_forward' 2>&1
    echo "--- guest HTTP paths (if curl is installed)"
    guest 'if command -v curl >/dev/null; then curl --noproxy "*" -sS --max-time 3 -I http://127.0.0.1:8080/; for address in $(ip -4 -o addr show dev eth0 | tr -s " " | cut -d " " -f 4 | cut -d / -f 1); do curl --noproxy "*" -sS --max-time 3 -I "http://$address:8080/"; done; else echo "guest curl unavailable"; fi' 2>&1
    echo "--- root nftables forwarding rules"
    "$vmctl" exec -u root smoke sh -c 'nft list ruleset' 2>&1 | tail -n 100
    echo "--- direct host HTTP path"
    host_get 2>&1 | head -c 512
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

step "boot with a volume"
"$vmctl" run -d --name smoke -v "$vol:/storage" --volume-size 20G "$image" sleep infinity
retry "the machine to accept exec" exec_up smoke

step "interactive exec: stdin reaches the guest and output returns"
reply=$(printf 'echo interactive-ok\nexit\n' | "$vmctl" exec -i smoke sh)
case "$reply" in *interactive-ok*) echo ok ;; *) fail "interactive exec answered: $reply" ;; esac

step "dockerd runs in nftables mode on the stock kernel"
retry "dockerd" dockerd_up
if ! guest 'docker info 2>&1 | grep -qi nftables'; then
  diagnose
  fail "Docker does not report the nftables backend; check the image configuration in docs/docker-in-machine.md."
fi
if ! "$vmctl" exec -u root smoke sh -c 'nft list tables | grep -q docker'; then
  diagnose
  fail "Docker nftables tables were not found by the root check (see docs/docker-in-machine.md). Record the dockerd log above, then evaluate the Kata kernel fallback (krun_set_kernel) if kernel support is missing."
fi
guest 'docker version --format "docker {{.Server.Version}}"'

step "the guest resolver can resolve Docker's registry"
dns_deadline=$((SECONDS + timeout_s))
until registry_dns_up >/dev/null 2>&1; do
  if [ "$SECONDS" -ge "$dns_deadline" ]; then
    diagnose
    fail "timed out waiting for guest registry DNS"
  fi
  sleep 1
done
echo "guest registry DNS resolved through libc"

step "the Docker daemon can pull nginx through its normal registry transport"
# DNS can become available before any external route. Retry the required pull,
# preserving daemon address selection and completed layers; create no container yet.
registry_deadline=$((SECONDS + timeout_s))
until guest 'docker pull nginx' > "$state/registry-pull.log" 2>&1; do
  if [ "$SECONDS" -ge "$registry_deadline" ]; then
    tail -n 20 "$state/registry-pull.log" >&2
    diagnose
    fail "Docker registry pull did not succeed within the retry window"
  fi
  sleep 5
done
cat "$state/registry-pull.log"

step "docker run -p 8080:80 nginx is reachable from the Mac at the guest IP"
guest 'docker run -d --name web -p 8080:80 nginx'
retry "nginx on the guest IP" nginx_up
echo "fetched nginx from the Mac"

step "containers reach the internet (DNS and TCP)"
guest 'docker run --rm busybox wget -qO- http://example.com | grep -qi "example domain"' \
  || fail "a container cannot reach the internet"

step "Compose CLI prerequisite inside the disposable guest"
if ! guest 'docker compose version' > "$state/compose-version.log" 2>&1; then
  [ "$(guest 'uname -m')" = aarch64 ] || fail "pinned Compose fixture requires a Linux aarch64 guest"
  compose_version=v2.40.3
  compose_sha=d26373b19e89160546d15407516cc59f453030d9bc5b43ba7faf16f7b4980137
  compose_binary=$state/docker-compose-linux-aarch64
  curl -fL --retry 3 "https://github.com/docker/compose/releases/download/$compose_version/docker-compose-linux-aarch64" -o "$compose_binary"
  python3 - "$compose_binary" "$compose_sha" <<'PYCOMPOSE'
import hashlib,sys
with open(sys.argv[1], 'rb') as binary:
    digest=hashlib.sha256()
    for chunk in iter(lambda: binary.read(1024*1024), b''):
        digest.update(chunk)
    actual=digest.hexdigest()
assert actual == sys.argv[2], f'Compose checksum mismatch: {actual}'
PYCOMPOSE
  "$vmctl" exec -i -u root smoke sh -c 'set -eu; destination=/usr/local/lib/docker/cli-plugins/docker-compose; test ! -e "$destination"; mkdir -p /usr/local/lib/docker/cli-plugins; cat > "$destination.tmp"; chmod 755 "$destination.tmp"; mv "$destination.tmp" "$destination"' < "$compose_binary"
  guest 'docker compose version'
else
  cat "$state/compose-version.log"
fi

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

step "stop and start keep the volume and Docker's data"
"$vmctl" exec -u root smoke sh -c 'echo persisted > /storage/marker && sync'
"$vmctl" stop smoke
"$vmctl" start smoke
retry "the machine to accept exec" exec_up smoke
retry "dockerd after restart" dockerd_up
# shellcheck disable=SC2016 # expanded in the guest
guest '[ "$(cat /storage/marker)" = persisted ]' || fail "volume marker (persisted) lost across stop/start"
guest 'docker image inspect nginx >/dev/null' || fail "Docker's data-root did not persist (nginx image gone)"
guest 'docker start web >/dev/null'
retry "nginx at the guest IP after restart" nginx_up

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
