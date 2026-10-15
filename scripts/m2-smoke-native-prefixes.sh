#!/usr/bin/env bash
# Actual guest RA/addressing and packets using an isolated prefix-capable helper.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
work=$(mktemp -d /tmp/clankerd-prefix-guests.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
export DYLD_FALLBACK_LIBRARY_PATH=${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}
cleanup() {
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
"$root/scripts/build-prefix-test-helper.sh" "$work/helper-build" > "$work/helper-build.log" 2>&1 || { tail -40 "$work/helper-build.log"; exit 1; }
# Verify invalid prefix/mode combinations fail during parsing, without root.
python3 - "$work/helper-build/vmnet-helper" <<'PYVALIDATE'
import subprocess,sys
helper=sys.argv[1]
for args in (['--nat66-prefix','2001:db8::'],['--nat66-prefix','fd42::1'],['--nat66-prefix','fd42::','--operation-mode','host']):
    result=subprocess.run([helper,*args,'--socket','/tmp/clankerd-prefix-argument-rejection.sock'],capture_output=True,text=True,timeout=3)
    assert result.returncode!=0 and '--nat66-prefix requires' in result.stderr,result
print('PASS prefix argument/mode validation before native startup')
PYVALIDATE
ifconfig -a > "$work/interfaces-before.txt"
python3 - "$work" <<'PYCONFIG'
from pathlib import Path
import ipaddress,json,re,secrets,shutil,sys
work=Path(sys.argv[1]);occupied=[]
for address,mask in re.findall(r'\binet ([0-9.]+) netmask (0x[0-9a-fA-F]+)',(work/'interfaces-before.txt').read_text()):
    occupied.append(ipaddress.IPv4Network((address,int(mask,16).bit_count()),strict=False))
free=[i for i in range(200,255) if not any(ipaddress.ip_network(f'192.168.{i}.0/24').overlaps(n) for n in occupied)]
assert len(free)>=2,'Need two unused /24 ranges'
stem='fd'+secrets.token_hex(1)+':'+secrets.token_hex(2)+':'+secrets.token_hex(2)
prefixes={f'192.168.{third}.1':f'{stem}:{i+1}::' for i,third in enumerate(free[:2])}
(work/'prefix-helper.json').write_text(json.dumps(dict(helper=str(work/'helper-build/vmnet-helper'),prefixes=prefixes)))
(work/'networks.tsv').write_text(''.join(f'{name}\t192.168.{third}.0/24\t{prefixes[f"192.168.{third}.1"]}\n' for name,third in zip(('prefix-one','prefix-two'),free[:2])))
PYCONFIG
cp "$root/scripts/fixtures/vmnet-prefix-helper-wrapper.py" "$work/prefix-helper.py"
chmod +x "$work/prefix-helper.py"
export CLANKERD_VMNET_HELPER=$work/prefix-helper.py
while IFS=$'\t' read -r network subnet prefix; do
  "$vmctl" network create "$network" --mode shared --subnet "$subnet" > "$work/$network-create.txt"
done < "$work/networks.tsv"
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
# Actual external IPv6 is required only when this same numeric endpoint works on Mac.
if python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" external 2606:4700:4700::1111 443 > "$work/mac-external6.log" 2>&1; then
  for name in prefix-a prefix-c; do "$vmctl" exec "$name" python3 -c "$server" external 2606:4700:4700::1111 443; done
else
  echo 'OUTSTANDING external IPv6/NAT66: numeric destination unavailable from Mac'
fi
echo 'PASS two native shared prefixes: actual RA/addressing, four-guest within-network packet matrix, Mac access, IPv4 NAT and restart'
echo 'COVERAGE: disposable patched helper only; production helper packaging, cross-network policy and IPv6 source enforcement remain open.'
