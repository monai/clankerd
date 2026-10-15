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
# Pick eight ranges which do not overlap active IPv4 interface networks.
python3 - "$work/interfaces-before.txt" > "$work/subnets.txt" <<'PY'
import ipaddress,re,sys
text=open(sys.argv[1]).read();occupied=[]
for address,mask in re.findall(r'\binet ([0-9.]+) netmask (0x[0-9a-fA-F]+)',text):
    occupied.append(ipaddress.IPv4Network((address,int(mask,16).bit_count()),strict=False))
free=[i for i in range(200,255) if not any(ipaddress.ip_network(f'192.168.{i}.0/24').overlaps(n) for n in occupied)]
assert len(free)>=8,'Need eight unused diagnostic /24 ranges; no interface changes made'
print('\n'.join(map(str,free[:8])))
PY
xcrun clang -Wall -Wextra -Werror -Wno-deprecated-declarations -fblocks \
  "$root/scripts/fixtures/vmnet-prefix-probe.c" -framework vmnet \
  -o "$work/probe" > "$work/build.log" 2>&1 || { cat "$work/build.log"; exit 1; }
subnets=()
while IFS= read -r subnet; do subnets+=("$subnet"); done < "$work/subnets.txt"
prefix=$(python3 -c 'import secrets;print("fd"+secrets.token_hex(1)+":"+secrets.token_hex(2)+":"+secrets.token_hex(2))')
# Shared cases coexist together. Optional host-prefix cases run alone so a
# stalled callback cannot strand the other successful cases before teardown.
: > "$work/results.jsonl"
for profile in shared host-default host-custom-a host-custom-b; do
  echo "Probe native prefix profile: $profile (each callback bounded to 15 seconds)"
  status=0
  sudo -n "$work/probe" "${subnets[@]}" "$prefix" "$profile" >> "$work/results.jsonl" 2>> "$work/native-metadata.log" || status=$?
  printf '%s\n' "$status" > "$work/$profile.status"
done
ifconfig -a > "$work/interfaces-after.txt"
netstat -rn > "$work/routes-after.txt"
python3 - "$work" <<'PY'
from pathlib import Path
import ipaddress,json,sys
root=Path(sys.argv[1]);rows=[json.loads(line) for line in (root/'results.jsonl').read_text().splitlines()]
starts={r['case']:r for r in rows if r['event']=='start'}
for i in range(8):
    r=starts.get(i)
    if r is None:
        print(f'case {i}: INCOMPLETE callback; inspect native-metadata.log')
        continue
    expected=r['requested_prefix']
    matched=bool(r['success'] and expected and r['prefix'] and ipaddress.IPv6Address(expected)==ipaddress.IPv6Address(r['prefix']))
    print(f"case {i} {r['mode']}: started={r['success']} requested={expected or 'automatic'} actual={r['prefix'] or 'none'} requested-prefix-honored={matched if expected else 'n/a'}")
for profile,indices in (('shared',range(4)),('host-default',range(4,6))):
    assert (root/(profile+'.status')).read_text().strip()=='0',f'{profile}: native startup/teardown failed; inspect native-metadata.log'
    assert all(i in starts for i in indices),f'{profile}: incomplete baseline'
assert all(starts[i]['success'] for i in (0,1,4,5)), 'Default attachment baseline failed'
custom=[starts[i] for i in (2,3)]
supported=all(r['success'] and r['prefix'] and ipaddress.IPv6Address(r['prefix'])==ipaddress.IPv6Address(r['requested_prefix']) for r in custom)
for profile,i in (('host-custom-a',6),('host-custom-b',7)):
    if (root/(profile+'.status')).read_text().strip()!='0' or i not in starts:
        print(f'LIMITATION: {profile} did not complete; isolated probe process exited, other profiles already torn down')
print(('SUPPORTED' if supported else 'UNSUPPORTED')+': requested distinct shared-mode IPv6 prefixes in this simultaneous native metadata probe')
print('COVERAGE: startup/metadata/teardown only; guest RA, packet delivery, NAT66 and isolation remain unproven for requested prefixes.')
PY
