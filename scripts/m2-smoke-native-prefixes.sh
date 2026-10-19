#!/usr/bin/env bash
# Actual guest RA/addressing and packets using persisted resource prefixes.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/artifact}
vmctl=$out/darwin-arm64/vmctl
work=$(mktemp -d /tmp/clankerd-prefix-guests.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn CLANKERD_GUEST_ROOT=$out/linux-arm64/management
cleanup() {
  if [ -n "${forward4:-}" ]; then sudo -n sysctl -w net.inet.ip.forwarding="$forward4" > "$work/forwarding-cleanup.log" 2>&1 || true; fi
  if [ -n "${forward6:-}" ]; then sudo -n sysctl -w net.inet6.ip6.forwarding="$forward6" >> "$work/forwarding-cleanup.log" 2>&1 || true; fi
  mkdir -p "$work/logs"
  python3 - "$work/state" "$work/logs" <<'PYRETAIN' || true
from pathlib import Path
import shutil,sys
source,target=map(Path,sys.argv[1:])
for path in source.rglob('*.log'):
    destination=target/path.relative_to(source);destination.parent.mkdir(parents=True,exist_ok=True)
    shutil.copy2(path,destination)
PYRETAIN
  for name in prefix-a prefix-b prefix-c prefix-d; do "$vmctl" rm -f "$name" >/dev/null 2>&1 || true; done
  for network in prefix-one prefix-two; do "$vmctl" network rm "$network" >/dev/null 2>&1 || true; done
  echo "Retained evidence: $work"
}
trap cleanup EXIT
echo "Evidence directory: $work"
sudo -n true
export CLANKERD_NETWORK=$out/darwin-arm64/clankerd-network
ifconfig -a > "$work/interfaces-before.txt"
python3 - "$work" <<'PYCONFIG'
from pathlib import Path
import ipaddress,json,re,sys
work=Path(sys.argv[1]);occupied=[]
for address,mask in re.findall(r'\binet ([0-9.]+) netmask (0x[0-9a-fA-F]+)',(work/'interfaces-before.txt').read_text()):
    occupied.append(ipaddress.IPv4Network((address,int(mask,16).bit_count()),strict=False))
free=[i for i in range(200,255) if not any(ipaddress.ip_network(f'192.168.{i}.0/24').overlaps(n) for n in occupied)]
assert len(free)>=2,'Need two unused /24 ranges'

(work/'networks.tsv').write_text(''.join(f'{name}\t192.168.{third}.0/24\n' for name,third in zip(('prefix-one','prefix-two'),free[:2])))
PYCONFIG
while IFS=$'\t' read -r network subnet prefix; do
  "$vmctl" network create --mode shared --subnet "$subnet" "$network" > "$work/$network-create.txt"
done < "$work/networks.tsv"
"$vmctl" network inspect prefix-one prefix-two > "$work/networks.json"
python3 - "$work/networks.json" > "$work/networks.tsv" <<'PYRESOURCES'
import json,sys
rows=json.load(open(sys.argv[1]));assert len({r['ipv6_prefix'] for r in rows})==2,rows
for row in rows:
    assert row['ipv6_prefix'],row
    print(row['name']+'\t'+row['subnet']['address']+'/'+str(row['subnet']['prefix_len'])+'\t'+row['ipv6_prefix'])
PYRESOURCES
[ "${CLANKERD_NETWORK_PF:-}" != off ] || { echo 'Production gate requires native PF policy enabled' >&2; exit 1; }
forward4=$(sysctl -n net.inet.ip.forwarding)
forward6=$(sysctl -n net.inet6.ip6.forwarding)
server=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
peer=$(cat "$root/scripts/fixtures/vmnet-isolation-peers.py")
for name in prefix-a prefix-b prefix-c prefix-d; do
  case "$name" in prefix-a|prefix-b) network=prefix-one;; *) network=prefix-two;; esac
  echo "Start $name in $network with native prefix selection"
  "$vmctl" run -d --name "$name" --network "$network" "${IMAGE:-python:3.13-alpine}" python3 -c "$server" server > "$work/$name-start.txt"
  passed=0
  for attempt in $(seq 1 40); do
    "$vmctl" inspect "$name" > "$work/$name.json"
    if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/$name.json" dual > "$work/$name-mac-probes.log" 2>&1; then passed=1; break; fi
    sleep 1
  done
  [ "$passed" = 1 ] || { tail -30 "$work/$name-mac-probes.log"; exit 1; }
  python3 - "$work/$name.json" "$work/networks.tsv" "$network" > "$work/$name-addresses.txt" <<'PYADDRESSES'
import ipaddress,json,sys
machine=json.load(open(sys.argv[1]))[0]
line=next(line.split() for line in open(sys.argv[2]) if line.split()[0]==sys.argv[3])
expected=ipaddress.ip_network(line[2]+'/64');addresses=machine['network']['addresses']
v4=next(a for a in addresses if ':' not in a)
v6=[a for a in addresses if ':' in a and not ipaddress.ip_address(a).is_link_local]
assert v6 and all(ipaddress.ip_address(a) in expected for a in v6),(expected,addresses)
assert ipaddress.ip_address(v4) in ipaddress.ip_network(line[1]),(line,v4)
print(v4);print(v6[0]);print(next(a+'%eth0' for a in addresses if ':' in a and ipaddress.ip_address(a).is_link_local))
PYADDRESSES
  "$vmctl" exec "$name" python3 -c 'import socket;s=socket.socket();s.settimeout(10);s.connect(("1.1.1.1",443));s.close();print("PASS outbound IPv4 TCP")' > "$work/$name-outbound4.log"
  echo "PASS $name native-prefix RA/addressing, Mac IPv4/IPv6/LL TCP/UDP, outbound IPv4"
done
# Exercise real Mac routing, rather than mistaking disabled forwarding for isolation.
sudo -n sysctl -w net.inet.ip.forwarding=1 net.inet6.ip6.forwarding=1 > "$work/forwarding.log"
for name in prefix-a prefix-b prefix-c prefix-d; do
  case "$name" in prefix-a|prefix-b) target=prefix-c;; *) target=prefix-a;; esac
  target6=$(sed -n '2p' "$work/$target-addresses.txt")
  gateway6=$(python3 -c 'import json,sys;print(next(a.split("%")[0] for a in json.load(open(sys.argv[1]))[0]["network"]["nameservers"] if a.startswith("fe80:")))' "$work/$name.json")
  target_prefix=$(python3 -c 'import ipaddress,sys;print(ipaddress.ip_network(sys.argv[1]+"/64",strict=False))' "$target6")
  "$vmctl" exec "$name" ip -6 route replace "$target_prefix" via "$gateway6" dev eth0 > "$work/$name-peer-route.log"
done
for pair in prefix-a,prefix-c prefix-c,prefix-a prefix-b,prefix-d prefix-d,prefix-b; do
  source=${pair%%,*}; target=${pair#*,}
  "$vmctl" exec "$source" python3 -c "$peer" 64 "$(sed -n '1p' "$work/$target-addresses.txt")" "$(sed -n '2p' "$work/$target-addresses.txt")" > "$work/$source-to-$target-blocked.jsonl"
  python3 - "$work/$source-to-$target-blocked.jsonl" <<'PYBOUNDARY'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==4 and all(not r['reached'] for r in rows),rows
assert all(any(error in r.get('error','').lower() for error in ('timed out','unreachable')) for r in rows),rows
PYBOUNDARY
  echo "PASS production policy blocks $source -> $target routed IPv4/IPv6 TCP/UDP"
done
bridge_one=$(/sbin/route -n get "$(sed -n '1p' "$work/prefix-a-addresses.txt")" | awk '/interface:/{print $2}')
bridge_two=$(/sbin/route -n get "$(sed -n '1p' "$work/prefix-c-addresses.txt")" | awk '/interface:/{print $2}')
[ -n "$bridge_one" ] && [ -n "$bridge_two" ] && [ "$bridge_one" != "$bridge_two" ]
sudo -n /sbin/pfctl -a com.apple/clankerd-networks -vvsr > "$work/production-pf-counters.log" 2>&1
python3 - "$work/production-pf-counters.log" "$bridge_one" "$bridge_two" <<'PYCOUNTERS'
import re,sys
text=open(sys.argv[1]).read()
for source,target in ((sys.argv[2],sys.argv[3]),(sys.argv[3],sys.argv[2])):
    label='clankerd-'+source+'-'+target
    block=next((b for b in re.split(r'(?m)^@',text) if 'label "'+label+'"' in b),None)
    assert block and int(re.search(r'Packets:\s*(\d+)',block).group(1))>0,(label,text)
print('PASS actual production PF block counters increased')
PYCOUNTERS
for name in prefix-a prefix-b prefix-c prefix-d; do
  python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/$name.json" dual > "$work/$name-post-policy-host.log" 2>&1
  "$vmctl" exec "$name" python3 -c 'import socket;s=socket.socket();s.settimeout(10);s.connect(("1.1.1.1",443));s.close()'
done
for pair in prefix-a,prefix-b prefix-b,prefix-a prefix-c,prefix-d prefix-d,prefix-c; do
  source=${pair%%,*}; target=${pair#*,}
  addresses=()
  while IFS= read -r address; do addresses+=("$address"); done < "$work/$target-addresses.txt"
  "$vmctl" exec "$source" python3 -c "$peer" 64 "${addresses[@]}" > "$work/$source-to-$target.jsonl"
  python3 - "$work/$source-to-$target.jsonl" <<'PYPEERS'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==6 and all(r['reached'] for r in rows),rows
PYPEERS
  echo "PASS $source -> $target IPv4/IPv6/LL TCP/UDP within selected network"
done
"$vmctl" stop prefix-a > "$work/restart.log"
# B must retain the network boundary while A is stopped.
"$vmctl" exec prefix-b python3 -c "$peer" 64 "$(sed -n '1p' "$work/prefix-c-addresses.txt")" > "$work/member-stop-boundary.jsonl"
python3 - "$work/member-stop-boundary.jsonl" <<'PYMEMBER'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==2 and all(not r['reached'] for r in rows),rows
PYMEMBER
"$vmctl" start prefix-a >> "$work/restart.log"
passed=0
for attempt in $(seq 1 40); do
  "$vmctl" inspect prefix-a > "$work/prefix-a-restarted.json"
  if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/prefix-a-restarted.json" dual > "$work/restart-probes.log" 2>&1; then passed=1; break; fi
  sleep 1
done
[ "$passed" = 1 ] || { tail -30 "$work/restart-probes.log"; exit 1; }
python3 - "$work/prefix-a.json" "$work/prefix-a-restarted.json" <<'PYRESTART'
import ipaddress,json,sys
before,after=[json.load(open(p))[0]['network']['addresses'] for p in sys.argv[1:]]
def prefixes(addresses):return {str(ipaddress.ip_network(a+'/64',strict=False)) for a in addresses if ':' in a and not ipaddress.ip_address(a).is_link_local}
assert prefixes(before) and prefixes(before)==prefixes(after),(before,after)
print('PASS native IPv6 prefix and packet connectivity survive machine restart')
PYRESTART
gateway6=$(python3 -c 'import json,sys;print(next(a.split("%")[0] for a in json.load(open(sys.argv[1]))[0]["network"]["nameservers"] if a.startswith("fe80:")))' "$work/prefix-a-restarted.json")
target_prefix=$(python3 -c 'import ipaddress,sys;print(ipaddress.ip_network(sys.argv[1]+"/64",strict=False))' "$(sed -n '2p' "$work/prefix-c-addresses.txt")")
"$vmctl" exec prefix-a ip -6 route replace "$target_prefix" via "$gateway6" dev eth0 > "$work/restarted-peer-route.log"
# Recovery must replace the registered helper without opening the network boundary.
id=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))[0]["id"])' "$work/prefix-a-restarted.json")
helper_pid_file=$work/state/machines/$id/net.pid
old_helper=$(cat "$helper_pid_file")
kill -KILL "$old_helper"
recovered=0
for attempt in $(seq 1 75); do
  sleep 2
  new_helper=$(cat "$helper_pid_file" 2>/dev/null || true)
  [ -n "$new_helper" ] && [ "$new_helper" != "$old_helper" ] || continue
  "$vmctl" inspect prefix-a > "$work/prefix-a-recovered.json"
  if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/prefix-a-recovered.json" dual > "$work/helper-recovery-probes.log" 2>&1; then recovered=1; break; fi
done
[ "$recovered" = 1 ] || { tail -30 "$work/helper-recovery-probes.log"; exit 1; }
"$vmctl" exec prefix-a python3 -c "$peer" 64 "$(sed -n '1p' "$work/prefix-c-addresses.txt")" "$(sed -n '2p' "$work/prefix-c-addresses.txt")" > "$work/recovered-boundary.jsonl"
python3 - "$work/recovered-boundary.jsonl" "$work/prefix-a-restarted.json" "$work/prefix-a-recovered.json" <<'PYRECOVERED'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==4 and all(not r['reached'] for r in rows),rows
before,after=[json.load(open(path))[0]['state']['pid'] for path in sys.argv[2:]]
assert before==after,(before,after)
PYRECOVERED
sudo -n /bin/cat /var/run/clankerd-network-pf/state.json > "$work/pf-state-after-recovery.json"
python3 - "$work/pf-state-after-recovery.json" "$old_helper" "$new_helper" <<'PYREGISTRY'
import json,sys
pids={row['pid'] for row in json.load(open(sys.argv[1]))['records'].values()}
assert int(sys.argv[2]) not in pids and int(sys.argv[3]) in pids,pids
print('PASS production PF registration and boundary survive member restart/helper replacement')
PYREGISTRY
# Reassignment changes one stopped member's attachment without recreating its workload.
if "$vmctl" update --network prefix-two prefix-a > "$work/update-running.log" 2>&1; then
  echo 'FAIL running network update unexpectedly succeeded' >&2; exit 1
fi
python3 -c 'import sys;assert "stop it first" in open(sys.argv[1]).read()' "$work/update-running.log"
for network in prefix-two prefix-one; do
  "$vmctl" stop prefix-a > "$work/update-$network.log"
  "$vmctl" update --network "$network" prefix-a >> "$work/update-$network.log"
  # Restart twice to verify the selected membership persists.
  for cycle in 1 2; do
    "$vmctl" start prefix-a >> "$work/update-$network.log"
    passed=0
    for attempt in $(seq 1 40); do
      "$vmctl" inspect prefix-a > "$work/prefix-a-updated.json"
      if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/prefix-a-updated.json" dual > "$work/update-$network-$cycle-probes.log" 2>&1; then passed=1; break; fi
      sleep 1
    done
    [ "$passed" = 1 ] || { tail -30 "$work/update-$network-$cycle-probes.log"; exit 1; }
    python3 - "$work/prefix-a-updated.json" "$work/networks.json" "$network" "$work/prefix-a.json" > "$work/prefix-a-addresses.txt" <<'PYUPDATED'
import ipaddress,json,sys
machine=json.load(open(sys.argv[1]))[0]
resource=next(r for r in json.load(open(sys.argv[2])) if r['name']==sys.argv[3])
before=json.load(open(sys.argv[4]))[0]
assert machine['id']==before['id'] and machine['config']==before['config'],machine
assert machine['host_config']['virtual_network']==resource['name'],machine
assert machine['host_config']['shared_ipv6_prefix']==resource['ipv6_prefix'],machine
addresses=machine['network']['addresses']
v4=next(a for a in addresses if ':' not in a)
v6=next(a for a in addresses if ':' in a and not ipaddress.ip_address(a).is_link_local)
assert ipaddress.ip_address(v4) in ipaddress.ip_network(resource['subnet']['address']+'/'+str(resource['subnet']['prefix_len']))
assert ipaddress.ip_address(v6) in ipaddress.ip_network(resource['ipv6_prefix']+'/64')
print(v4);print(v6);print(next(a+'%eth0' for a in addresses if ':' in a and ipaddress.ip_address(a).is_link_local))
PYUPDATED
    case "$network" in prefix-two) allowed=prefix-c; excluded=prefix-b;; *) allowed=prefix-b; excluded=prefix-c;; esac
    gateway6=$(python3 -c 'import json,sys;print(next(a.split("%")[0] for a in json.load(open(sys.argv[1]))[0]["network"]["nameservers"] if a.startswith("fe80:")))' "$work/prefix-a-updated.json")
    target_prefix=$(python3 -c 'import ipaddress,sys;print(ipaddress.ip_network(sys.argv[1]+"/64",strict=False))' "$(sed -n '2p' "$work/$excluded-addresses.txt")")
    "$vmctl" exec prefix-a ip -6 route replace "$target_prefix" via "$gateway6" dev eth0
    for pair in "prefix-a,$allowed,allowed" "$allowed,prefix-a,allowed" "prefix-a,$excluded,blocked" "$excluded,prefix-a,blocked"; do
      IFS=, read -r source target expectation <<< "$pair"
      addresses=()
      while IFS= read -r address; do addresses+=("$address"); done < "$work/$target-addresses.txt"
      if [ "$expectation" = blocked ]; then unset 'addresses[2]'; fi
      result=$work/update-$network-$cycle-$source-to-$target.jsonl
      "$vmctl" exec "$source" python3 -c "$peer" 64 "${addresses[@]}" > "$result"
      python3 - "$result" "$expectation" <<'PYUPDATEDPEERS'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
allowed=sys.argv[2]=='allowed'
assert len(rows)==(6 if allowed else 4) and all(r['reached']==allowed for r in rows),rows
if not allowed:assert all(any(e in r.get('error','').lower() for e in ('timed out','unreachable')) for r in rows),rows
PYUPDATEDPEERS
    done
    sudo -n /bin/cat /var/run/clankerd-network-pf/state.json > "$work/update-pf-state.json"
    python3 - "$work/update-pf-state.json" "$work/networks.json" "$network" "$work/run/${id:0:12}.net" <<'PYUPDATEDREGISTRY'
import json,sys
resource=next(r for r in json.load(open(sys.argv[2])) if r['name']==sys.argv[3])
rows=[r for r in json.load(open(sys.argv[1]))['records'].values() if r['socket']==sys.argv[4]]
assert len(rows)==1 and rows[0]['network_id']==resource['id'],rows
PYUPDATEDREGISTRY
    "$vmctl" exec prefix-a python3 -c 'import socket;s=socket.socket();s.settimeout(10);s.connect(("1.1.1.1",443));s.close()'
    if [ "$cycle" = 1 ]; then "$vmctl" stop prefix-a >> "$work/update-$network.log"; fi
  done
  echo "PASS stopped update to $network: persisted membership, peer access, routed exclusion, Mac/NAT and PF identity"
done
# Actual external IPv6 is required only when this same numeric endpoint works on Mac.
if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" external 2606:4700:4700::1111 443 > "$work/mac-external6.log" 2>&1; then
  for name in prefix-a prefix-c; do "$vmctl" exec "$name" python3 -c "$server" external 2606:4700:4700::1111 443; done
else
  echo 'OUTSTANDING external IPv6/NAT66: numeric destination unavailable from Mac'
fi
# Last-member teardown must remove these registrations without touching other workloads.
for name in prefix-a prefix-b prefix-c prefix-d; do "$vmctl" rm -f "$name" > "$work/$name-remove.log"; done
sudo -n /bin/cat /var/run/clankerd-network-pf/state.json > "$work/pf-state-after-cleanup.json"
python3 - "$work/pf-state-after-cleanup.json" "$work/run/" <<'PYCLEANUP'
import json,sys
state=json.load(open(sys.argv[1]))
assert not any(row['socket'].startswith(sys.argv[2]) for row in state['records'].values()),state
if not state['records']:assert state['token'] is None,state
print('PASS production PF last-member cleanup and owned reference release')
PYCLEANUP
echo 'PASS production network lifecycle: native prefixes, routed IPv4/IPv6 exclusion, same-network peers, Mac/NAT, member stop, restart, helper replacement, stopped network update and teardown'
echo 'COVERAGE: trusted Mac host retaining active runtime policy; cross-network other-VMM, sleep/wake and independent LAN-peer acceptance remain open.'
