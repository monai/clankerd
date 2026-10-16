#!/usr/bin/env bash
set -Eeuo pipefail
# This prototype owns its test anchor and measures an unfiltered baseline.
export CLANKERD_NETWORK_PF=off
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
export DYLD_FALLBACK_LIBRARY_PATH=${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}
work=$(mktemp -d /tmp/clankerd-isolation-path.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
captures=()
pf_anchor=com.apple/clankerd-isolation-test-$$
pf_token=''
pf_loaded=0
host_pid=''
ipv6_aliases=()
stop_captures() {
  for pid in "${captures[@]}"; do sudo -n kill -INT "$pid" 2>/dev/null || true; done
  for pid in "${captures[@]}"; do wait "$pid" 2>/dev/null || true; done
  captures=()
}
cleanup() {
  stop_captures
  if [ -n "$host_pid" ]; then kill "$host_pid" 2>/dev/null || true; wait "$host_pid" 2>/dev/null || true; fi
  if [ "$pf_loaded" = 1 ]; then sudo -n /sbin/pfctl -a "$pf_anchor" -F rules > "$work/pf-cleanup.log" 2>&1 || true; fi
  if [ -n "$pf_token" ]; then sudo -n /sbin/pfctl -X "$pf_token" >> "$work/pf-cleanup.log" 2>&1 || true; fi
  for entry in "${ipv6_aliases[@]}"; do
    interface=${entry%%,*}; address=${entry#*,}
    sudo -n /sbin/ifconfig "$interface" inet6 "$address" -alias >> "$work/ipv6-cleanup.log" 2>&1 || true
  done
  python3 - "$work/state" "$work/retained-state" <<'PYRETAIN' || true
from pathlib import Path
import shutil,sys
source,destination=map(Path,sys.argv[1:])
for path in source.rglob('*'):
    if path.is_file() and path.suffix in ('.log','.json'):
        target=destination/path.relative_to(source)
        target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copy2(path,target)
PYRETAIN
  for name in path-a path-c path-d; do "$vmctl" rm -f "$name" >/dev/null 2>&1 || true; done
  echo "Retained evidence: $work"
}
trap cleanup EXIT
echo "Evidence directory: $work"
sudo -n true
server=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
peer=$(cat "$root/scripts/fixtures/vmnet-isolation-peers.py")
python3 -c "$server" server > "$work/mac-listeners.log" 2>&1 &
host_pid=$!
sleep 1
kill -0 "$host_pid"
for name in path-a path-c path-d; do
  case "$name" in path-a) subnet=192.168.105.0/24; singleton=0;; path-c) subnet=192.168.106.0/24; singleton=1;; path-d) subnet=192.168.107.0/24; singleton=0;; esac
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
d=$(cat "$work/path-d-ip")
bridge_a=$(route -n get "$a" | awk '/interface:/ {print $2}')
bridge_c=$(route -n get "$c" | awk '/interface:/ {print $2}')
bridge_d=$(route -n get "$d" | awk '/interface:/ {print $2}')
sysctl net.inet.ip.forwarding net.inet6.ip6.forwarding | tee "$work/forwarding.txt"
netstat -rn > "$work/mac-routes.txt"
prefix=$(printf 'fd42:%x' "$(( $$ % 65535 ))")
a6=$prefix:1::2
c6=$prefix:2::2
d6=$prefix:3::2
for name in path-a path-c path-d; do
  case "$name" in path-a) segment=1; interface=$bridge_a;; path-c) segment=2; interface=$bridge_c;; path-d) segment=3; interface=$bridge_d;; esac
  gateway6=$prefix:$segment::1
  ipv6_aliases+=("$interface,$gateway6")
  sudo -n /sbin/ifconfig "$interface" inet6 "$gateway6/64" alias
  "$vmctl" exec "$name" ip -6 addr add "$prefix:$segment::2/64" dev eth0
  for remote in 1 2 3; do
    [ "$remote" != "$segment" ] || continue
    "$vmctl" exec "$name" ip -6 route add "$prefix:$remote::/64" via "$gateway6" dev eth0
  done
  echo "$name diagnostic IPv6=$prefix:$segment::2 gateway=$gateway6 interface=$interface"
done
sleep 3
sysctl net.inet6.ip6.forwarding > "$work/ipv6-forwarding-live.txt"
cat "$work/ipv6-forwarding-live.txt"
netstat -rn -f inet6 > "$work/ipv6-routes-live.txt"
sudo -n /sbin/pfctl -sr > "$work/pf-root-rules.txt" 2> "$work/pf-root-errors.txt"
if ! grep -q 'anchor "com.apple/\*"' "$work/pf-root-rules.txt"; then
  echo 'Required existing com.apple/* PF anchor absent; no firewall changes made.'
  exit 1
fi
probe_peers() {
  phase=$1
  echo "Peer probes: $phase"
  for name in path-a path-c path-d; do
    if [ "$name" = path-c ]; then target=$a; target6=$a6; else target=$c; target6=$c6; fi
    "$vmctl" exec "$name" python3 -c "$peer" 64 "$target" "$target6" | tee "$work/$phase-$name.jsonl"
  done
  "$vmctl" exec path-c python3 -c "$peer" 64 "$d" "$d6" | tee "$work/$phase-c-to-d.jsonl"
}
controls() {
  phase=$1
  for name in path-a path-c path-d; do
    "$vmctl" inspect "$name" > "$work/$name.json"
    python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/$name.json" ipv4-services > "$work/$phase-$name-mac.log" 2>&1
    "$vmctl" exec "$name" python3 -c 'import socket;s=socket.socket();s.settimeout(15);s.connect(("1.1.1.1",443));print("PASS outbound IPv4 TCP 1.1.1.1:443");s.close()'
    echo "PASS $phase Mac -> $name IPv4 TCP/UDP"
    case "$name" in path-a) gateway=192.168.105.1;; path-c) gateway=192.168.106.1;; path-d) gateway=192.168.107.1;; esac
    local_gateway=$(python3 - "$work/$name.json" <<'PYGATEWAY'
import json,sys
network=json.load(open(sys.argv[1]))[0]['network']
print(next(address.split('%')[0]+'%eth0' for address in network['nameservers'] if address.startswith('fe80:')))
PYGATEWAY
)
    "$vmctl" exec "$name" python3 -c "$peer" 64 "$gateway" "$local_gateway" > "$work/$phase-$name-to-mac.jsonl"
    python3 - "$work/$phase-$name-to-mac.jsonl" <<'PYHOSTCHECK'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==4 and all(row['reached'] for row in rows),rows
print('PASS guest -> own Mac gateway IPv4/IPv6-link-local TCP/UDP')
PYHOSTCHECK
  done
  for name in path-a path-d; do
    if [ "$name" = path-a ]; then target=$d; else target=$a; fi
    "$vmctl" exec "$name" python3 -c "$peer" 64 "$target" > "$work/$phase-ordinary-$name.jsonl"
    python3 - "$work/$phase-ordinary-$name.jsonl" <<'PYORDINARY'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==2 and all(row['reached'] for row in rows),rows
print('PASS ordinary A/D peer IPv4 TCP/UDP')
PYORDINARY
  done
}
probe_peers before
controls before
printf 'block drop in quick on %s inet to 192.168.106.0/24\nblock drop in quick on %s inet to 192.168.106.0/24\nblock drop in quick on %s inet to { 192.168.105.0/24, 192.168.107.0/24 }\n' "$bridge_a" "$bridge_d" "$bridge_c" > "$work/pf-test.rules"
printf 'block drop in quick on %s inet6 to %s:2::/64\nblock drop in quick on %s inet6 to %s:2::/64\nblock drop in quick on %s inet6 to { %s:1::/64, %s:3::/64 }\n' "$bridge_a" "$prefix" "$bridge_d" "$prefix" "$bridge_c" "$prefix" "$prefix" >> "$work/pf-test.rules"
sudo -n /sbin/pfctl -n -a "$pf_anchor" -f "$work/pf-test.rules" > "$work/pf-validation.log" 2>&1
sudo -n /sbin/pfctl -E > "$work/pf-enable.log" 2>&1
pf_token=$(awk '/Token/ {print $NF}' "$work/pf-enable.log")
[ -n "$pf_token" ] || { echo 'PF reference token missing; inspect pf-enable.log'; exit 1; }
pf_loaded=1
sudo -n /sbin/pfctl -a "$pf_anchor" -f "$work/pf-test.rules" > "$work/pf-load.log" 2>&1
sudo -n /sbin/pfctl -k "$a" -k "$c" > "$work/pf-clear-test-state.log" 2>&1
sudo -n /sbin/pfctl -k "$c" -k "$a" >> "$work/pf-clear-test-state.log" 2>&1
sudo -n /sbin/pfctl -k "$d" -k "$c" >> "$work/pf-clear-test-state.log" 2>&1
sudo -n /sbin/pfctl -k "$c" -k "$d" >> "$work/pf-clear-test-state.log" 2>&1
for source6 in "$a6" "$d6"; do
  sudo -n /sbin/pfctl -k "$source6" -k "$c6" >> "$work/pf-clear-test-state.log" 2>&1
  sudo -n /sbin/pfctl -k "$c6" -k "$source6" >> "$work/pf-clear-test-state.log" 2>&1
done
probe_peers enforced
controls enforced
# Exercise guest-controlled source addresses before relying on exact-IP rules.
"$vmctl" exec path-a ip addr add 192.168.105.250/24 dev eth0
"$vmctl" exec path-a env EVIDENCE_SOURCE=192.168.105.250 python3 -c "$peer" 64 "$c" | tee "$work/alias-exact-rules.jsonl"
echo 'Repeat alias-source probes with interface rules:'
"$vmctl" exec path-a env EVIDENCE_SOURCE=192.168.105.250 python3 -c "$peer" 64 "$c" | tee "$work/alias-subnet-rules.jsonl"
controls interface-enforced
"$vmctl" exec path-a ip addr del 192.168.105.250/24 dev eth0
sudo -n /sbin/pfctl -a "$pf_anchor" -vvsr > "$work/pf-counters.txt" 2>&1
cat "$work/pf-counters.txt"
sudo -n /sbin/pfctl -a "$pf_anchor" -F rules > "$work/pf-remove.log" 2>&1
pf_loaded=0
probe_peers removed
controls removed
python3 - "$work" <<'PYRESULT'
import json,sys
from pathlib import Path
root=Path(sys.argv[1])
for phase,expected in [('before',True),('enforced',False),('removed',True)]:
    rows=[json.loads(line) for name in ('path-a','path-c','path-d','c-to-d') for line in (root/(phase+'-'+name+'.jsonl')).read_text().splitlines()]
    passed=len(rows)==16 and all(row['reached']==expected for row in rows)
    print(phase+': '+('PASS' if passed else 'FAIL')+' required peer behavior')
    assert passed,rows
PYRESULT
