#!/usr/bin/env bash
# macOS15 shared membership/isolation gate; requires prior vmnet setup gate.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" = 15 ] || { echo 'Requires macOS15' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
work=$(mktemp -d /tmp/clankerd-shared.XXXXXX)
export CLANKERD_STATE_DIR=$work/state
export CLANKERD_RUNTIME_DIR=$work/run
failed=0
cleanup() {
  for name in shared-a shared-b shared-isolated shared-separate; do
    "$vmctl" rm -f "$name" >/dev/null 2>&1 || true
  done
  if [ "$failed" = 1 ]; then echo "Retained diagnostics: $work" >&2; else rm -rf "$work"; fi
}
trap cleanup EXIT
trap 'failed=1; echo "FAIL line $LINENO: $BASH_COMMAND" >&2; find "$work/state" -name "*.log" -exec tail -50 {} \;' ERR
server=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
peer=$(cat "$root/scripts/fixtures/vmnet-shared-peers.py")
image=${IMAGE:-python:3.13-alpine}
"$vmctl" run -d --name shared-a "$image" python3 -c "$server" server
"$vmctl" run -d --name shared-b "$image" python3 -c "$server" server
"$vmctl" run -d --name shared-isolated --shared-network 192.168.105.0/24 --isolate-peers "$image" python3 -c "$server" server
"$vmctl" run -d --name shared-separate --shared-network "${VMNET_SEPARATE_NETWORK:-192.168.112.0/24}" "$image" python3 -c "$server" server
inspect() { "$vmctl" inspect "$1" > "$work/$1.json"; }
host_access() {
  local name=$1 passed=0
  for attempt in $(seq 1 20); do
    inspect "$name"
    if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/$name.json" dual > "$work/$name-probes.log" 2>&1; then passed=1; break; fi
    sleep 1
  done
  cat "$work/$name-probes.log"
  [ "$passed" = 1 ]
}
peer_access() {
  local source=$1 target=$2 expected=$3 v4 v6 v6ll
  inspect "$target"
  read -r v4 v6 v6ll < <(python3 "$root/scripts/fixtures/vmnet-shared-peers.py" targets "$work/$target.json")
  echo "$source -> $target: $expected"
  "$vmctl" exec "$source" python3 -c "$peer" peer "$v4" "$v6" "$expected"
  if [ "$v6ll" != "$v6" ]; then
    "$vmctl" exec "$source" python3 -c "$peer" peer "$v4" "$v6ll" "$expected"
  fi
}
for name in shared-a shared-b shared-isolated shared-separate; do host_access "$name"; done
peer_access shared-a shared-b allowed
peer_access shared-b shared-a allowed
for name in shared-a shared-b; do
  peer_access "$name" shared-isolated blocked
  peer_access shared-isolated "$name" blocked
done
peer_access shared-a shared-separate blocked
peer_access shared-separate shared-a blocked
# Verify isolated shared NAT independently, through an IPv4-only socket.
"$vmctl" exec shared-isolated python3 -c 'import socket; s=socket.socket(socket.AF_INET,socket.SOCK_STREAM); s.settimeout(15); s.connect(("1.1.1.1",443)); print("PASS isolated IPv4 external TCP"); s.close()'
"$vmctl" stop shared-isolated
"$vmctl" start shared-isolated
host_access shared-isolated
peer_access shared-a shared-isolated blocked
peer_access shared-isolated shared-a blocked
python3 - "$work/shared-isolated.json" <<'PY'
import json,sys
config=json.load(open(sys.argv[1]))[0]['host_config']
assert config['shared_peer_participation'] is False
assert config['shared_network']=={'address':'192.168.105.0','prefix_len':24}
print('PASS persisted selected network and isolation after restart')
PY
for name in shared-a shared-b shared-isolated shared-separate; do "$vmctl" stop "$name"; done
[ -z "$(find "$work/run" -name '*.net' -print)" ]
[ -z "$(find "$work/state" -name net.pid -print)" ]
echo 'PASS common/selected shared networks, direct IPv4+IPv6 TCP/UDP, bidirectional isolation, Mac access, IPv4 NAT, restart and cleanup'
echo 'OUTSTANDING ticket09: other-VMM interoperability/isolation, user-responder mDNS, and external IPv6 on an IPv6-capable host'
