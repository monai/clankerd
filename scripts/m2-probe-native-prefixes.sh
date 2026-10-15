#!/usr/bin/env bash
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d /tmp/clankerd-native-prefixes.XXXXXX)
trap 'echo "Retained evidence: $work"' EXIT
echo "Evidence directory: $work"
sudo -n true
sw_vers > "$work/host.txt"
ifconfig -a > "$work/interfaces-before.txt"
netstat -rn > "$work/routes-before.txt"
# Pick six ranges which do not overlap active IPv4 interface networks.
python3 - "$work/interfaces-before.txt" > "$work/subnets.txt" <<'PY'
import ipaddress,re,sys
text=open(sys.argv[1]).read();occupied=[]
for address,mask in re.findall(r'\binet ([0-9.]+) netmask (0x[0-9a-fA-F]+)',text):
    occupied.append(ipaddress.IPv4Network((address,int(mask,16).bit_count()),strict=False))
free=[i for i in range(200,255) if not any(ipaddress.ip_network(f'192.168.{i}.0/24').overlaps(n) for n in occupied)]
assert len(free)>=6,'Need six unused diagnostic /24 ranges; no interface changes made'
print('\n'.join(map(str,free[:6])))
PY
xcrun clang -Wall -Wextra -Werror -Wno-deprecated-declarations -fblocks \
  "$root/scripts/fixtures/vmnet-prefix-probe.c" -framework vmnet \
  -o "$work/probe" > "$work/build.log" 2>&1 || { cat "$work/build.log"; exit 1; }
subnets=()
while IFS= read -r subnet; do subnets+=("$subnet"); done < "$work/subnets.txt"
prefix=$(python3 -c 'import secrets;print("fd"+secrets.token_hex(1)+":"+secrets.token_hex(2)+":"+secrets.token_hex(2))')
echo 'Probe six simultaneous attachments: default shared, distinct-prefix shared, distinct-prefix host-only'
status=0
sudo -n "$work/probe" "${subnets[@]}" "$prefix" > "$work/results.jsonl" 2> "$work/native-metadata.log" || status=$?
ifconfig -a > "$work/interfaces-after.txt"
netstat -rn > "$work/routes-after.txt"
python3 - "$work/results.jsonl" "$status" <<'PY'
import ipaddress,json,sys
rows=[json.loads(line) for line in open(sys.argv[1])]
starts={r['case']:r for r in rows if r['event']=='start'}
assert len(starts)==6,('Incomplete native capability matrix; see native-metadata.log',starts)
for i,r in sorted(starts.items()):
    expected=r['requested_prefix']
    matched=bool(r['success'] and expected and r['prefix'] and ipaddress.IPv6Address(expected)==ipaddress.IPv6Address(r['prefix']))
    print(f"case {i} {r['mode']}: started={r['success']} requested={expected or 'automatic'} actual={r['prefix'] or 'none'} requested-prefix-honored={matched if expected else 'n/a'}")
assert sys.argv[2]=='0','Native probe or teardown failed; inspect metadata and stop events'
assert all(starts[i]['success'] for i in (0,1)), 'Default shared baseline failed'
custom=[starts[i] for i in (2,3)]
supported=all(r['success'] and r['prefix'] and ipaddress.IPv6Address(r['prefix'])==ipaddress.IPv6Address(r['requested_prefix']) for r in custom)
print(('SUPPORTED' if supported else 'UNSUPPORTED')+': requested distinct shared-mode IPv6 prefixes in this simultaneous native metadata probe')
print('COVERAGE: startup/metadata/teardown only; guest RA, packet delivery, NAT66 and isolation remain unproven for requested prefixes.')
PY
