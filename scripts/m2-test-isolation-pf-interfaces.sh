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
  case "$name" in path-a) subnet=192.168.105.0/24; isolation=();; path-c) subnet=192.168.106.0/24; isolation=(--isolate-peers);; path-d) subnet=192.168.107.0/24; isolation=();; esac
  "$vmctl" run -d --name "$name" --shared-network "$subnet" "${isolation[@]}" "${IMAGE:-python:3.13-alpine}" python3 -c "$server" server > "$work/$name-start.log"
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
sudo -n /sbin/pfctl -sr > "$work/pf-root-rules.txt" 2> "$work/pf-root-errors.txt"
if ! grep -q 'anchor "com.apple/\*"' "$work/pf-root-rules.txt"; then
  echo 'Required existing com.apple/* PF anchor absent; no firewall changes made.'
  exit 1
fi
probe_peers() {
  phase=$1
  echo "Peer probes: $phase"
  for name in path-a path-c path-d; do
    if [ "$name" = path-c ]; then target=$a; else target=$c; fi
    "$vmctl" exec "$name" python3 -c "$peer" 64 "$target" | tee "$work/$phase-$name.jsonl"
  done
  "$vmctl" exec path-c python3 -c "$peer" 64 "$d" | tee "$work/$phase-c-to-d.jsonl"
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
    rows=[json.loads(line) for name in ('path-a','path-c','path-d') for line in (root/(phase+'-'+name+'.jsonl')).read_text().splitlines()]
    print(phase+': '+('PASS' if len(rows)==6 and all(row['reached']==expected for row in rows) else 'FAIL')+' required peer behavior')
PYRESULT
