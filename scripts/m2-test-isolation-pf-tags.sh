#!/usr/bin/env bash
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
export DYLD_FALLBACK_LIBRARY_PATH=${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}
work=$(mktemp -d /tmp/clankerd-pf-tags.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
captures=()
pf_anchor=com.apple/clankerd-tag-test-$$
pf_token=''
pf_loaded=0
host_pid=''
ipv6_aliases=()
owned_routes=()
owned_routes4=()
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
  for address in "${owned_routes4[@]}"; do
    sudo -n /sbin/route -n delete -inet -host "$address" >> "$work/route-cleanup.log" 2>&1 || true
  done
  for address in "${owned_routes[@]}"; do
    sudo -n /sbin/route -n delete -inet6 -host "$address" >> "$work/route-cleanup.log" 2>&1 || true
  done
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
  for network in tag-net-a tag-net-c tag-net-d; do "$vmctl" network rm "$network" >/dev/null 2>&1 || true; done
  echo "Retained evidence: $work"
}
trap cleanup EXIT
echo "Evidence directory: $work"
sudo -n true
printf 'anchor "syntax-only" in on lo0 all tag "clankerd_syntax_probe"\nblock drop out quick on lo0 all tagged "clankerd_syntax_probe"\n' > "$work/pf-syntax.rules"
if ! sudo -n /sbin/pfctl -n -a "$pf_anchor" -f "$work/pf-syntax.rules" > "$work/pf-syntax.log" 2>&1; then
  cat "$work/pf-syntax.log"
  echo 'FAIL tag syntax rejected before starting VMs or changing PF'
  exit 1
fi
echo 'PASS tag-rule syntax validation; starting positive packet baseline'
export TAG_FIXTURE_ROOT=$root
server=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
peer=$(cat "$root/scripts/fixtures/vmnet-isolation-peers.py")
python3 -c "$server" server > "$work/mac-listeners.log" 2>&1 &
host_pid=$!
sleep 1
kill -0 "$host_pid"
for pair in tag-net-a,192.168.105.0/24 tag-net-c,192.168.106.0/24 tag-net-d,192.168.107.0/24; do
  "$vmctl" network create "${pair%%,*}" --mode shared --subnet "${pair#*,}"
done
for name in path-a path-c path-d; do
  case "$name" in path-a) network=tag-net-a;; path-c) network=tag-net-c;; path-d) network=tag-net-d;; esac
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
# Guest-controlled source aliases: actual delivery before filtering is required.
arp_probe=$(cat <<'PYARP'
import socket,struct,time,sys
candidate=socket.inet_aton(sys.argv[1])
mac=bytes.fromhex(open('/sys/class/net/eth0/address').read().strip().replace(':',''))
s=socket.socket(socket.AF_PACKET,socket.SOCK_RAW,socket.htons(0x0806));s.bind(('eth0',0));s.settimeout(.2)
probe=b'\xff'*6+mac+b'\x08\x06'+struct.pack('!HHBBH',1,0x0800,6,4,1)+mac+b'\0'*4+b'\0'*6+candidate
for _ in range(3):
    s.send(probe);deadline=time.monotonic()+1
    while time.monotonic()<deadline:
        try: frame=s.recv(2048)
        except socket.timeout: continue
        if len(frame)>=42 and frame[22:28]!=mac:
            assert frame[28:32]!=candidate and not (frame[28:32]==b'\0'*4 and frame[38:42]==candidate), 'ARP address conflict: '+sys.argv[1]
print('PASS RFC5227-style ARP probe: '+sys.argv[1])
PYARP
)
for entry in path-a,192.168.105.250 path-c,192.168.106.250 path-d,192.168.107.250; do
  name=${entry%%,*}; address=${entry#*,}
  "$vmctl" exec "$name" python3 -c "$arp_probe" "$address" > "$work/$name-arp-conflict-check.log"
  "$vmctl" exec "$name" ip addr add "$address/24" dev eth0
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
    "$vmctl" exec "$name" python3 -c "$peer" 64 "$target" "$target6" > "$work/$phase-$name.jsonl"
  done
  "$vmctl" exec path-c python3 -c "$peer" 64 "$d" "$d6" > "$work/$phase-c-to-d.jsonl"
  for name in path-a path-c path-d; do
    case "$name" in path-a) source4=192.168.105.250; target4=$c;; path-c) source4=192.168.106.250; target4=$a;; path-d) source4=192.168.107.250; target4=$c;; esac
    "$vmctl" exec "$name" env EVIDENCE_SOURCE="$source4" python3 -c "$peer" 64 "$target4" > "$work/$phase-alias-$name.jsonl"
  done
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
  python3 - "$a6" "$c6" "$d6" > "$work/$phase-mac-alias6.log" <<'PYMAC6'
import importlib.util,sys
# Imported fixture path is supplied through the shell's environment below.
from pathlib import Path
root=Path(__import__('os').environ['TAG_FIXTURE_ROOT'])
spec=importlib.util.spec_from_file_location('services',root/'scripts/fixtures/vmnet-ipv6-services.py')
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
for address in sys.argv[1:]: module.probe(address)
print('PASS Mac -> all three native bridges distinct-prefix IPv6 TCP/UDP')
PYMAC6
  echo 'PASS Mac -> all three native bridges distinct-prefix IPv6 TCP/UDP'
  for name in path-a path-d; do
    if [ "$name" = path-a ]; then target=$d; target6=$d6; common_else target=$a; target6=$a6; common_fi
    "$vmctl" exec "$name" python3 -c "$peer" 64 "$target" "$target6" > "$work/$phase-ordinary-$name.jsonl"
    python3 - "$work/$phase-ordinary-$name.jsonl" <<'PYORDINARY'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==4 and all(row['reached'] for row in rows),rows
print('PASS ordinary A/D peer IPv4 and distinct-prefix IPv6 TCP/UDP')
PYORDINARY
  done
}
# Validate all positive before-paths immediately; no failed baseline can become
# an apparent isolation success after installing rules.
check_peers() {
  phase=$1; expected=$2
  python3 - "$work" "$phase" "$expected" <<'PYCHECK'
import json,sys
from pathlib import Path
root,phase,expected=Path(sys.argv[1]),sys.argv[2],sys.argv[3]=='true'
paths=[root/(phase+'-'+name+'.jsonl') for name in ('path-a','path-c','path-d','c-to-d','alias-path-a','alias-path-c','alias-path-d')]
rows=[json.loads(line) for path in paths for line in path.read_text().splitlines()]
assert len(rows)==22, len(rows)
assert all(row['reached']==expected for row in rows),rows
if not expected:
    assert all(any(message in row.get('error','').lower() for message in ('timed out','no route to host','host is unreachable')) for row in rows),rows
print('PASS '+phase+': '+str(len(rows))+' actual peer TCP/UDP probes; IPv4 source aliases and distinct IPv6 prefixes')
PYCHECK
}
probe_peers before
check_peers before true
controls before
# Empty anchor rules may carry tags without selecting a pass/block action.
# Apple XNU pf_test_rule applies r->tag before traversing r->anchor.
# NO fallback to pass rules: unsupported grammar fails before firewall mutation.
cat > "$work/pf-test.rules" <<PFRULES
anchor "mark-a" in on $bridge_a all tag "clankerd_tag_a_$$"
anchor "mark-c" in on $bridge_c all tag "clankerd_tag_c_$$"
anchor "mark-d" in on $bridge_d all tag "clankerd_tag_d_$$"
block drop out quick on $bridge_c all tagged "clankerd_tag_a_$$" label "tag-c-from-a"
block drop out quick on $bridge_c all tagged "clankerd_tag_d_$$" label "tag-c-from-d"
block drop out quick on $bridge_a all tagged "clankerd_tag_c_$$" label "tag-a-from-c"
block drop out quick on $bridge_d all tagged "clankerd_tag_c_$$" label "tag-d-from-c"
PFRULES
# Retain the installed primary grammar documentation for any syntax failure.
MANPAGER=cat man 5 pf.conf > "$work/mac-pf.conf-man.txt" 2>&1 || true
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
for source4 in 192.168.105.250 192.168.106.250 192.168.107.250; do
  sudo -n /sbin/pfctl -k "$source4" >> "$work/pf-clear-test-state.log" 2>&1
  sudo -n /sbin/pfctl -k 0.0.0.0/0 -k "$source4" >> "$work/pf-clear-test-state.log" 2>&1
done
probe_peers enforced
check_peers enforced false
controls enforced
sudo -n /sbin/pfctl -a "$pf_anchor" -vvsr > "$work/pf-counters.txt" 2>&1
python3 - "$work/pf-counters.txt" <<'PYCOUNTERS'
import re,sys
text=open(sys.argv[1]).read()
for label in ('tag-c-from-a','tag-c-from-d','tag-a-from-c','tag-d-from-c'):
    match=next((block for block in re.split(r'(?m)^@',text) if 'label "'+label+'"' in block),None)
    assert match is not None, (label,text)
    count=re.search(r'Packets:\s*(\d+)',match)
    assert count and int(count.group(1))>0, (label,match)
    print('PASS native PF block counter '+label+': '+count.group(1)+' packets')
PYCOUNTERS
sudo -n /sbin/pfctl -a "$pf_anchor" -F rules > "$work/pf-remove.log" 2>&1
pf_loaded=0
probe_peers removed
controls removed
check_peers removed true
echo 'PASS decision-neutral anchor-tag prototype: actual tag boundary behavior, Mac/NAT and ordinary networks retained.'
echo 'NOT PROVEN: overlapping-prefix IPv6 alias routing (failed baseline), production host-policy coexistence under every custom rule/state, malicious raw Ethernet, multicast policy, external IPv6.'
