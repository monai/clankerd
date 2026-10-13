#!/usr/bin/env bash
# Real macOS 15 gate: family filtering is installed before the VMM boots.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" = 15 ] || { echo 'Requires macOS 15' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export VMNET_TEST_REAL_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_VMSPAWN=$root/scripts/fixtures/vmnet-family-filter.py
export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
export CLANKERD_VMNET_HELPER=${CLANKERD_VMNET_HELPER:-/opt/vmnet-helper/bin/vmnet-helper}
[ -x "$CLANKERD_VMNET_HELPER" ] || { echo 'Run scripts/m2-smoke-vmnet.sh to install vmnet-helper first' >&2; exit 1; }
sudo --non-interactive -- "$CLANKERD_VMNET_HELPER" --version
sw_vers
python3 "$CLANKERD_VMSPAWN" --self-test
work=$(mktemp -d /tmp/clankerd-ipv6.XXXXXX)
export CLANKERD_STATE_DIR=$work/state
export CLANKERD_RUNTIME_DIR=$work/run
failed=0
cleanup() {
  for name in vmnet-dual vmnet-ipv4 vmnet-ipv6; do
    "$vmctl" rm -f "$name" >/dev/null 2>&1 || true
  done
  if [ "$failed" = 1 ]; then
    echo "Retained diagnostics: $work" >&2
  else
    rm -rf "$work"
  fi
}
trap cleanup EXIT
trap 'failed=1; echo "FAIL line $LINENO: $BASH_COMMAND" >&2; find "$work/state" -name "*.log" -exec tail -60 {} \;' ERR
services=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
external_address=${VMNET_EXTERNAL_IPV6:-2606:4700:4700::1111}
external_port=${VMNET_EXTERNAL_PORT:-443}
host_external=0
if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" external "$external_address" "$external_port"; then
  host_external=1
else
  echo 'HOST LIMITATION: same IPv6 destination unavailable from Mac; external IPv6 acceptance remains outstanding'
fi
for family in dual ipv4 ipv6; do
  export VMNET_TEST_FAMILY=$family
  name=vmnet-$family
  "$vmctl" run -d --name "$name" "${IMAGE:-python:3.13-alpine}" python3 -c "$services" server
  find "$work/state" -name vmnet-helper.log -exec tail -25 {} \;
  "$vmctl" exec "$name" sh -c '! command -v nft'
  sleep 3 # Allow initial RA/DAD and service listeners to settle.
  passed=0
  for attempt in $(seq 1 30); do
    "$vmctl" inspect "$name" > "$work/$family.json"
    if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/$family.json" "$family" > "$work/$family-probes.log" 2>&1; then
      passed=1
      break
    fi
    sleep 1
  done
  cat "$work/$family-probes.log"
  [ "$passed" = 1 ]
  if [ "$family" != ipv4 ] && [ "$host_external" = 1 ]; then
    external_passed=0
    for attempt in $(seq 1 10); do
      if "$vmctl" exec "$name" python3 -c "$services" external "$external_address" "$external_port"; then
        external_passed=1
        break
      fi
      sleep 3
    done
    [ "$external_passed" = 1 ]
  fi
  "$vmctl" stop "$name"
  [ -z "$(find "$work/run" -name '*.net' -print)" ]
  [ -z "$(find "$work/state" -name net.pid -print)" ]
  if [ "$family" = dual ]; then
    "$vmctl" start "$name"
    "$vmctl" exec "$name" python3 -c 'print("PASS vsock management after restart")'
    "$vmctl" stop "$name"
  fi
  "$vmctl" rm "$name"
done
if [ "$host_external" = 1 ]; then
  echo 'PASS macOS 15: dual/single-family startup, direct IPv4/IPv6 TCP+UDP, scoped IPv6, external IPv6, restart, no nft and cleanup'
else
  echo 'PASS local macOS 15 IPv4/IPv6 and single-family startup; OUTSTANDING external IPv6 (host probe failed)'
fi
