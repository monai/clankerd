#!/usr/bin/env bash
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
export DYLD_FALLBACK_LIBRARY_PATH=${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}
work=$(mktemp -d /tmp/clankerd-isolation-path.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
captures=()
stop_captures() {
  for pid in "${captures[@]}"; do sudo -n kill -INT "$pid" 2>/dev/null || true; done
  for pid in "${captures[@]}"; do wait "$pid" 2>/dev/null || true; done
  captures=()
}
cleanup() {
  stop_captures
  for name in path-a path-c; do "$vmctl" rm -f "$name" >/dev/null 2>&1 || true; done
  echo "Retained evidence: $work"
}
trap cleanup EXIT
echo "Evidence directory: $work"
sudo -n true
server=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
peer=$(cat "$root/scripts/fixtures/vmnet-isolation-peers.py")
for name in path-a path-c; do
  if [ "$name" = path-a ]; then subnet=192.168.105.0/24; singleton=0; else subnet=192.168.106.0/24; singleton=1; fi
  network="network-${subnet//[.\/]/-}"
  if [ "$singleton" = 1 ]; then network="singleton-$name"; fi
  if ! "$vmctl" network inspect "$network" >/dev/null 2>&1; then
    if [ "$singleton" = 1 ]; then "$vmctl" network create "$network"; else "$vmctl" network create --subnet "$subnet" "$network"; fi
  fi
  "$vmctl" run -d --name "$name" --network "$network" "${IMAGE:-python:3.13-alpine}" python3 -c "$server" server > "$work/$name-start.log"
  ready=0
  for attempt in $(seq 1 30); do
    "$vmctl" inspect "$name" > "$work/$name.json"
    if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/$name.json" ipv4-services > "$work/$name-health.log" 2>&1; then ready=1; break; fi
    sleep 1
  done
  [ "$ready" = 1 ] || { tail -12 "$work/$name-health.log"; exit 1; }
  python3 - "$work/$name.json" > "$work/$name-ip" <<'PY'
import json,sys
print(next(a for a in json.load(open(sys.argv[1]))[0]['network']['addresses'] if ':' not in a))
PY
done
a=$(cat "$work/path-a-ip")
c=$(cat "$work/path-c-ip")
sysctl net.inet.ip.forwarding net.inet6.ip6.forwarding | tee "$work/forwarding.txt"
netstat -rn > "$work/mac-routes.txt"
interfaces=()
for address in "$a" "$c"; do
  route -n get "$address" > "$work/route-$address.txt"
  interface=$(awk '/interface:/ {print $2}' "$work/route-$address.txt")
  interfaces+=("$interface")
  ifconfig "$interface" > "$work/$interface.txt"
  sudo -n /usr/sbin/tcpdump -U -n -i "$interface" -w "$work/$interface.pcap" "host $a and host $c" > "$work/$interface-capture.log" 2>&1 &
  captures+=("$!")
done
sleep 1
for pid in "${captures[@]}"; do kill -0 "$pid"; done
"$vmctl" exec path-a python3 -c 'from pathlib import Path;print(Path("/proc/net/route").read_text())' > "$work/guest-a-routes.txt"
echo "Testing A=$a -> isolated C=$c IPv4 TCP/UDP hop64 and hop1"
for hops in 64 1; do
  "$vmctl" exec path-a python3 -c "$peer" "$hops" "$c" | tee "$work/probes-$hops.jsonl"
done
stop_captures
for interface in "${interfaces[@]}"; do
  echo "Packets on $interface (IP TTL and Ethernet headers):"
  sudo -n /usr/sbin/tcpdump -nn -e -v -r "$work/$interface.pcap" > "$work/$interface-packets.txt" 2>/dev/null
  head -24 "$work/$interface-packets.txt"
done
echo 'Full packet captures, Mac routes, guest routes and probe results retained.'
