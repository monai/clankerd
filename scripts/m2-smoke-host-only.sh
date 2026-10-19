#!/usr/bin/env bash
# Diagnostic only: actual normal-hop packet matrix, no host policy changes.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" = 15 ] || exit 1
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/artifact}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn CLANKERD_GUEST_ROOT=$out/linux-arm64/management
work=$(mktemp -d /tmp/clankerd-isolation-evidence.XXXXXX)
echo "Evidence directory: $work"
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
host_pid=''
names='evidence-a evidence-b evidence-c evidence-d evidence-e'
secondary() {
  CLANKERD_STATE_DIR=$work/conflicting-state CLANKERD_RUNTIME_DIR=$work/conflicting-run "$vmctl" "$@"
}
cleanup() {
  secondary rm -f host-only-conflict >/dev/null 2>&1 || true
  secondary network rm other >/dev/null 2>&1 || true
  if [ -n "$host_pid" ]; then kill "$host_pid" 2>/dev/null || true; wait "$host_pid" 2>/dev/null || true; fi
  python3 - "$work/state" "$work/retained-state" <<'PYLOGS' || true
from pathlib import Path
import shutil,sys
source,destination=map(Path,sys.argv[1:])
for path in source.rglob('*'):
    if path.is_file() and path.suffix in ('.log','.json'):
        target=destination/path.relative_to(source)
        target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copy2(path,target)
PYLOGS
  for name in $names; do "$vmctl" rm -f "$name" >/dev/null 2>&1 || true; done
  echo "Retained evidence: $work"
}
trap cleanup EXIT
sysctl net.inet.ip.forwarding net.inet6.ip6.forwarding > "$work/mac-forwarding-before.txt"
netstat -rn > "$work/mac-routes-before.txt"
cat "$work/mac-forwarding-before.txt"
multicast=$(cat "$root/scripts/fixtures/vmnet-multicast-evidence.py")
server=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
server="$multicast
${server/def server():/def server():$'\n'    multicast_listen()}"
peer=$(cat "$root/scripts/fixtures/vmnet-isolation-peers.py")
image=${IMAGE:-python:3.13-alpine}
python3 - "$root/scripts/fixtures/vmnet-ipv6-services.py" "$work/host-ready" <<'PYHOST' &
import importlib.util,socket,sys,threading,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('services',sys.argv[1]);services=importlib.util.module_from_spec(spec);spec.loader.exec_module(services)
sockets=[]
for family in (socket.AF_INET,socket.AF_INET6):
    for kind,port in ((socket.SOCK_STREAM,19081),(socket.SOCK_DGRAM,19082)):
        sock=socket.socket(family,kind)
        if family==socket.AF_INET6:
            sock.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1)
            if kind==socket.SOCK_DGRAM:sock.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_RECVPKTINFO,1)
        sock.bind(('::' if family==socket.AF_INET6 else '0.0.0.0',port))
        if kind==socket.SOCK_STREAM:sock.listen()
        sockets.append(sock)
        def serve(sock=sock,kind=kind,family=family):
            while True:
                if kind==socket.SOCK_STREAM:
                    connection,_=sock.accept()
                    with connection:
                        try:
                            while data:=connection.recv(2048):connection.sendall(data)
                        except (BrokenPipeError,ConnectionResetError):pass
                elif family==socket.AF_INET6:services.echo_ipv6_datagram(sock)
                else:
                    data,peer=sock.recvfrom(2048);sock.sendto(data,peer)
        threading.Thread(target=serve,daemon=True).start()
Path(sys.argv[2]).touch()
while True:time.sleep(60)
PYHOST
host_pid=$!
for attempt in $(seq 1 50); do [ ! -e "$work/host-ready" ] || break; kill -0 "$host_pid"; sleep 0.1; done
[ -e "$work/host-ready" ]
"$vmctl" network create --mode shared --subnet 192.168.105.0/24 shared > "$work/networks.log"
"$vmctl" network create --mode shared --subnet 192.168.107.0/24 other >> "$work/networks.log"
"$vmctl" network create --mode host-only --subnet 192.168.128.0/24 local >> "$work/networks.log"
healthy=()
for name in $names; do
  case "$name" in
    evidence-a|evidence-b) subnet=192.168.105.0/24; isolation=(--network shared);;
    evidence-c|evidence-e) subnet=192.168.128.0/24; isolation=(--network local);;
    evidence-d) subnet=192.168.107.0/24; isolation=(--network other);;
  esac
  if ! "$vmctl" run -d --name "$name" "${isolation[@]}" "$image" env EVIDENCE_NAME="$name" python3 -c "$server" server > "$work/$name-start.log" 2>&1; then
    echo "$name START FAILED (details retained)"; continue
  fi
  passed=0
  for attempt in $(seq 1 30); do
    "$vmctl" inspect "$name" > "$work/$name.json"
    if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/$name.json" ipv4-services > "$work/$name-host.log" 2>&1; then passed=1; break; fi
    sleep 1
  done
  if [ "$passed" != 1 ]; then echo "$name HOST HEALTH FAILED (details retained)"; continue; fi
  if ! python3 - "$work/$name.json" > "$work/$name-addresses.txt" <<'PYTARGET'
import ipaddress,json,sys
network=json.load(open(sys.argv[1]))[0]['network']
addresses=[d['address'] for d in network['address_details'] if not d['tentative'] and not d['dad_failed'] and not d['deprecated']]
v4=next(a for a in addresses if ipaddress.ip_address(a).version==4)
global6=next((a for a in addresses if ipaddress.ip_address(a).version==6 and not ipaddress.ip_address(a).is_link_local),'none')
local6=next(a for a in addresses if ipaddress.ip_address(a).version==6 and ipaddress.ip_address(a).is_link_local)
print(v4,global6,local6+'%eth0')
PYTARGET
  then echo "$name ADDRESS COVERAGE MISSING"; continue; fi
  "$vmctl" exec "$name" python3 -c "$peer" 64 127.0.0.1 ::1 > "$work/$name-local-listeners.jsonl"
  if ! python3 - "$work/$name-local-listeners.jsonl" <<'PYLISTEN'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==4 and all(row['reached'] for row in rows), rows
PYLISTEN
  then echo "$name LOCAL LISTENER HEALTH FAILED"; continue; fi
  "$vmctl" exec "$name" sh -c 'test -f /tmp/multicast-received.jsonl || touch /tmp/multicast-received.jsonl'
  healthy+=("$name")
  echo "$name HOST IPv4 TCP/UDP PASS addresses: $(cat "$work/$name-addresses.txt")"
done
[ "${#healthy[@]}" = 5 ] || { echo "FAIL incomplete multicast coverage: ${#healthy[@]}/5 healthy VMs"; exit 1; }
# Warm and measure each source's actual Mac path before peer observations.
sysctl net.inet.ip.forwarding net.inet6.ip6.forwarding > "$work/mac-forwarding-live.txt"
netstat -rn > "$work/mac-routes-live.txt"
for name in "${healthy[@]}"; do
  case "$name" in evidence-a|evidence-b) gateway=192.168.105.1;; evidence-c|evidence-e) gateway=192.168.128.1;; evidence-d) gateway=192.168.107.1;; esac
read -r isolated_v4 _ < "$work/$name-addresses.txt"
bridge=$(route -n get "$isolated_v4" | awk '/interface:/ {print $2}')
ifconfig "$bridge" > "$work/$name-mac-interface.txt"
python3 - "$work/$name-mac-interface.txt" > "$work/$name-mac-v6.txt" <<'PYMAC'
import ipaddress,sys
for line in open(sys.argv[1]):
    words=line.split()
    if words and words[0]=='inet6':
        address=words[1].split('%')[0]
        if not ipaddress.ip_address(address).is_loopback:
            print(address+('%eth0' if ipaddress.ip_address(address).is_link_local else ''))
PYMAC
mac_v6=()
while IFS= read -r address; do mac_v6+=("$address"); done < "$work/$name-mac-v6.txt"
  [ "${#mac_v6[@]}" -gt 0 ] || echo "$name -> Mac IPv6 NOT PROVEN: no interface address"
  "$vmctl" exec "$name" env EVIDENCE_PORT_BASE=19081 python3 -c "$peer" 64 "$gateway" "${mac_v6[@]}" > "$work/$name-to-mac.jsonl"
  printf "%s -> " "$name"
  python3 "$root/scripts/fixtures/vmnet-isolation-peers.py" summary "$work/$name-to-mac.jsonl"
done

for source in "${healthy[@]}"; do
  "$vmctl" exec "$source" python3 -c "$multicast
multicast_send('$source')"
done
sleep 2
for target in "${healthy[@]}"; do
  "$vmctl" exec "$target" sh -c 'cat /tmp/multicast-received.jsonl' > "$work/$target-multicast.jsonl"
  python3 - "$target" "$work/$target-multicast.jsonl" <<'PYMULTICAST'
import json,socket,sys
observed={}
for line in open(sys.argv[2]):
    row=json.loads(line)
    data=bytes.fromhex(row['dns_packet_hex'])
    name=data[13:13+data[12]].decode()
    observed.setdefault('IPv4' if row['family']==socket.AF_INET else 'IPv6',set()).add(name)
print(sys.argv[1]+' received: '+str({family:sorted(names) for family,names in observed.items()}))
group=('evidence-a','evidence-b') if sys.argv[1] in ('evidence-a','evidence-b') else (('evidence-c','evidence-e') if sys.argv[1] in ('evidence-c','evidence-e') else (sys.argv[1],))
expected={'clankerd-evidence-'+name for name in group}
for family in ('IPv4','IPv6'):
    assert observed.get(family,set())==expected,(sys.argv[1],family,observed,expected)
PYMULTICAST
done
for target in "${healthy[@]}"; do
  read -r v4 _ < "$work/$target-addresses.txt"
  case "$target" in evidence-a|evidence-b) gateway=192.168.105.1;; evidence-c|evidence-e) gateway=192.168.128.1;; evidence-d) gateway=192.168.107.1;; esac
  python3 - "$root/scripts/fixtures/vmnet-multicast-evidence.py" "$target" "$v4" "$gateway" <<'PYDISCOVER'
import runpy,sys
scope=runpy.run_path(sys.argv[1])
scope['discover'](sys.argv[2],sys.argv[3],True,sys.argv[4])
PYDISCOVER
done
for source in "${healthy[@]}"; do
  for target in "${healthy[@]}"; do
    read -r v4 _ < "$work/$target-addresses.txt"
    expected=False
    if [ "$source" = "$target" ] || { [[ "$source" = evidence-[ab] ]] && [[ "$target" = evidence-[ab] ]]; } || { [[ "$source" = evidence-[ce] ]] && [[ "$target" = evidence-[ce] ]]; }; then expected=True; fi
    "$vmctl" exec "$source" python3 -c "$multicast
discover('$target','$v4',$expected)" > "$work/$source-$target-discovery.log"
  done
  echo "PASS $source mDNS A self/allowed-peer discovery and isolated-peer exclusion"
done
python3 - <<'PYEXTERNAL'
import socket
with socket.socket(socket.AF_INET,socket.SOCK_STREAM) as connection:
    connection.settimeout(15);connection.connect(('1.1.1.1',443))
print('PASS Mac IPv4 external TCP 1.1.1.1:443')
PYEXTERNAL
for name in evidence-c evidence-e; do
  "$vmctl" inspect "$name" > "$work/$name-host-only.json"
  python3 - "$work/$name-host-only.json" <<'PYPOLICY'
import json,sys
machine=json.load(open(sys.argv[1]))[0]
assert machine['host_config']['network_mode']=='host-only',machine['host_config']
assert machine['host_config']['virtual_network']=='local',machine['host_config']
network=machine['network']
assert not network['ipv4']['default_route'] and not network['ipv6']['default_route'],network
print('PASS persisted host-only network membership and absent IPv4/IPv6 default routes')
PYPOLICY
  "$vmctl" exec "$name" python3 -c 'import socket;s=socket.socket();s.settimeout(5)
try:
 s.connect(("1.1.1.1",443))
except OSError as error:
 print("PASS no built-in outbound IPv4 TCP:",error)
else:
 raise AssertionError("Host-only unexpectedly reached 1.1.1.1:443")
finally:
 s.close()'
done
for source in evidence-c evidence-e; do
  if [ "$source" = evidence-c ]; then target=evidence-e; else target=evidence-c; fi
  read -r v4 _ ll6 < "$work/$target-addresses.txt"
  "$vmctl" exec "$source" python3 -c "$peer" 64 "$v4" "$ll6" > "$work/$source-$target-peers.jsonl"
  python3 - "$work/$source-$target-peers.jsonl" <<'PYMEMBERS'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==4 and all(row['reached'] for row in rows),rows
print('PASS host-only members IPv4/scoped IPv6 TCP/UDP')
PYMEMBERS
done
# Distinct resources in independent state roots must not merge on a native bridge.
"$vmctl" network inspect local > "$work/host-only-network.json"
subnet=$(python3 -c 'import json,sys;n=json.load(open(sys.argv[1]))[0]["subnet"];print(n["address"]+"/"+str(n["prefix_len"]))' "$work/host-only-network.json")
secondary network create --mode host-only --subnet "$subnet" other > "$work/conflicting-network.json"
if secondary run -d --name host-only-conflict --network other "$image" sh -c 'echo UNEXPECTED_WORKLOAD; sleep 60' > "$work/conflicting-start.log" 2>&1; then
  echo 'FAIL distinct host-only resources merged across state roots'
  exit 1
fi
python3 - "$work/conflicting-start.log" <<'PYCONFLICT'
import sys
text=open(sys.argv[1]).read()
assert 'native bridge conflicts with an active virtual network' in text,text
PYCONFLICT
secondary inspect host-only-conflict > "$work/conflicting-machine.json"
python3 - "$work/conflicting-machine.json" "$work/conflicting-state" <<'PYNOLAUNCH'
import json,sys
from pathlib import Path
machine=json.load(open(sys.argv[1]))[0]
assert machine['state']['pid'] is None,machine['state']
assert not list(Path(sys.argv[2]).rglob('net.pid'))
PYNOLAUNCH
secondary logs host-only-conflict > "$work/conflicting-workload.log" 2>&1 || true
if python3 -c 'import sys;sys.exit(0 if "UNEXPECTED_WORKLOAD" in open(sys.argv[1]).read() else 1)' "$work/conflicting-workload.log"; then echo 'FAIL conflicting workload was launched'; exit 1; fi
secondary rm -f host-only-conflict > "$work/conflicting-remove.log"
secondary network rm other > "$work/conflicting-network-remove.log"
# Recheck the permitted peer path after rejecting the conflicting registration.
read -r member4 _ member6 < "$work/evidence-e-addresses.txt"
"$vmctl" exec evidence-c python3 -c "$peer" 64 "$member4" "$member6" > "$work/after-conflict-peers.jsonl"
python3 - "$work/after-conflict-peers.jsonl" <<'PYUNCHANGED'
import json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
assert len(rows)==4 and all(row['reached'] for row in rows),rows
print('PASS resource identity rejects cross-state host-only collision before workload, preserving existing peers')
PYUNCHANGED
if "$vmctl" network rm local > "$work/in-use-removal.log" 2>&1; then echo 'FAIL removed network with live members'; exit 1; fi
echo 'PASS network removal rejects live membership'
"$vmctl" stop evidence-c > "$work/restart-stop.log"
"$vmctl" start evidence-c > "$work/restart-start.log"
"$vmctl" exec evidence-c python3 -c 'print("PASS host-only vsock after restart")'
"$vmctl" inspect evidence-c > "$work/restarted.json"
python3 - "$work/restarted.json" <<'PYRESTART'
import json,sys
machine=json.load(open(sys.argv[1]))[0]
assert machine['host_config']['network_mode']=='host-only'
assert machine['host_config']['virtual_network']=='local'
print('PASS host-only configuration survives restart')
PYRESTART
qemu-system-aarch64 --version > "$work/qemu-version.txt"
qemu-system-aarch64 -machine virt -netdev help > "$work/qemu-netdevs.txt" 2>&1 || true
limactl list --json > "$work/lima-instances.json" 2> "$work/lima-list.log" || true
cat "$work/qemu-version.txt" "$work/qemu-netdevs.txt" "$work/lima-instances.json"
echo 'PASS host-only virtual-network members communicate, Mac services/mDNS, absent default routes, outbound rejection and restart; cross-network host-only routing remains unverified.'
