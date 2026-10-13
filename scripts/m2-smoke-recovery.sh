#!/usr/bin/env bash
# Real macOS 15 recovery without changing the workload or its selected network.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" = 15 ] || { echo 'Requires macOS 15' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_NET_DIAGNOSTICS=1
export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
export CLANKERD_VMNET_HELPER=${CLANKERD_VMNET_HELPER:-/opt/vmnet-helper/bin/vmnet-helper}
export DYLD_FALLBACK_LIBRARY_PATH=${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}
work=$(mktemp -d /tmp/clankerd-recovery.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
name=vmnet-recovery
failed=0
host_pid=''
cleanup() {
  "$vmctl" rm -f "$name" >/dev/null 2>&1 || true
  if [ -n "$host_pid" ]; then kill "$host_pid" 2>/dev/null || true; wait "$host_pid" 2>/dev/null || true; fi
  if [ "$failed" = 1 ]; then echo "Retained diagnostics: $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT
trap 'failed=1; find "$work/state" -name "*.log" -exec tail -100 {} \;' ERR
fixture=$root/scripts/fixtures/vmnet-recovery-services.py
python3 "$fixture" unix-server "$work/host.sock" &
host_pid=$!
services=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
recovery=$(cat "$fixture")
"$vmctl" run -d -i --name "$name" "${IMAGE:-python:3.13-alpine}" python3 -c "$services
$recovery" recovery-server
"$vmctl" inspect "$name" > "$work/initial.json"
id=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))[0]["id"])' "$work/initial.json")
short=${id:0:12}
dir=$work/state/machines/$id
python3 "$fixture" bind "$work/run/$short.tunnels" "$work/host.sock"
workload_pid=$("$vmctl" exec "$name" cat /tmp/recovery-workload.pid)
check() {
  "$vmctl" inspect "$name" > "$work/current.json"
  python3 - "$work/initial.json" "$work/current.json" "$1" <<'PY'
import json,sys
before=json.load(open(sys.argv[1]))[0]
after=json.load(open(sys.argv[2]))[0]
assert after['state']['status']=='running', after
assert after['state']['started_at']==before['state']['started_at'], 'workload restarted'
assert after['host_config']==before['host_config'], 'selected network policy changed'
network=after['network']
assert network is not None, after
if sys.argv[3]=='down':
    assert not network['ready'], network
    assert not network['ipv4']['local_available'] and not network['ipv6']['local_available'], network
else:
    assert network['ready'] and network['ipv4']['local_available'], network
print('PASS observed connectivity',sys.argv[3])
PY
}
wait_for() {
  for attempt in $(seq 1 "$2"); do
    if check "$1" > "$work/check.log" 2>&1; then cat "$work/check.log"; return; fi
    sleep 0.2
  done
  cat "$work/check.log" >&2; return 1
}
management() {
  [ "$("$vmctl" exec "$name" cat /tmp/recovery-workload.pid)" = "$workload_pid" ]
  "$vmctl" exec "$name" python3 -c "$recovery" guest-unix
  printf 'recovery-exec\n' | "$vmctl" exec -i "$name" python3 -c 'import sys;assert sys.stdin.read()=="recovery-exec\n";print("PASS exec stdin/output")'
  previous_signal=$("$vmctl" exec "$name" python3 -c 'from pathlib import Path;p=Path("/tmp/recovery-signal");print(p.read_text() if p.exists() else "none")')
  "$vmctl" kill --signal USR1 "$name"
  delivered=0
  for attempt in $(seq 1 10); do
    current_signal=$("$vmctl" exec "$name" cat /tmp/recovery-signal)
    if [ "$current_signal" != "$previous_signal" ]; then delivered=1; break; fi
    sleep 0.1
  done
  [ "$delivered" = 1 ]
  python3 "$fixture" attach "$work/run/$short.sock"
}
packets() {
  "$vmctl" inspect "$name" > "$work/packet.json"
  python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/packet.json" ipv4-services
}
wait_for up 100
packets
# Real link/address loss is held throughout management and Unix binding probes.
"$vmctl" exec "$name" ip link set eth0 down
"$vmctl" exec "$name" ip -4 addr flush dev eth0
wait_for down 20
management
"$vmctl" exec "$name" ip link set eth0 up
wait_for up 150
packets
# dhcpcd is independently supervised; its death must not restart the workload.
"$vmctl" exec "$name" python3 -c "$recovery" kill-client
wait_for down 10
wait_for up 150
management
packets
# Detached native supervision reconnects the same VMM endpoint after helper death.
for cycle in 1 2; do
  old_helper=$(cat "$dir/net.pid")
  kill -KILL "$old_helper"
  wait_for down 10
  management
  wait_for up 150
  new_helper=$(cat "$dir/net.pid")
  [ "$old_helper" != "$new_helper" ]
  packets
  [ "$(find "$work/run" -name '*.relay' | wc -l | tr -d ' ')" = 1 ]
  [ "$(find "$work/run" -name '*.relay-peer' | wc -l | tr -d ' ')" = 1 ]
done
"$vmctl" stop "$name"
[ -z "$(find "$work/run" \( -name '*.net' -o -name '*.relay' -o -name '*.relay-peer' \) -print)" ]
[ -z "$(find "$work/state" -name net.pid -print)" ]
"$vmctl" rm "$name"
echo 'PASS real VM link/address loss, configuration-client restart, repeated native helper replacement, stable workload/vsock/Unix binding and cleanup; host sleep/wake and external IPv6 remain unverified'
