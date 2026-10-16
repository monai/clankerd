#!/usr/bin/env bash
# Real macOS 15 bridge gate; fixture selection never changes production defaults.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" = 15 ] || { echo 'Requires macOS 15' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export VMNET_TEST_LIBRARY_PATH=${VMNET_TEST_LIBRARY_PATH:-${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}}
export VMNET_TEST_REAL_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_VMSPAWN=$root/scripts/fixtures/vmnet-family-filter.py
export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
export CLANKERD_VMNET_HELPER=${CLANKERD_VMNET_HELPER:-/opt/vmnet-helper/bin/vmnet-helper}
sudo --non-interactive -- "$CLANKERD_VMNET_HELPER" --version
sw_vers
work=$(mktemp -d /tmp/clankerd-bridge.XXXXXX)
export CLANKERD_STATE_DIR=$work/state
export CLANKERD_RUNTIME_DIR=$work/run
failed=0
cleanup() {
  if [ "$failed" = 1 ]; then
    python3 - "$work" <<'PYLOG'
from pathlib import Path
import shutil,sys
work=Path(sys.argv[1])
for log in (work/'state').rglob('*.log'):
 target=work/'diagnostics'/log.relative_to(work/'state')
 target.parent.mkdir(parents=True,exist_ok=True)
 shutil.copy2(log,target)
PYLOG
  fi
  "$vmctl" rm -f bridge-smoke bridge-unavailable >/dev/null 2>&1 || true
  if [ "$failed" = 1 ]; then echo "Retained bridge diagnostics: $work" >&2; else rm -rf "$work"; fi
}
trap cleanup EXIT
trap 'failed=1; echo "FAIL line $LINENO: $BASH_COMMAND" >&2; find "$work/state" -name "*.log" -exec tail -50 {} \;' ERR
sudo --non-interactive -- "$CLANKERD_VMNET_HELPER" --list-shared-interfaces > "$work/supported"
cat "$work/supported"
python3 - "$work/supported" > "$work/active" <<'PY'
import subprocess,sys
for interface in open(sys.argv[1]).read().splitlines():
 interface=interface.strip()
 if not interface: continue
 result=subprocess.run(['/sbin/ifconfig',interface],capture_output=True,text=True)
 if result.returncode==0 and any(line.strip()=='status: active' for line in result.stdout.splitlines()):
  print(interface)
PY
[ -s "$work/active" ] || { echo 'No supported active physical LAN interface; bridge packet acceptance outstanding' >&2; exit 1; }
services=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
external_probe='import ipaddress,socket,subprocess,sys
address=sys.argv[1];family=socket.AF_INET6 if ipaddress.ip_address(address).version==6 else socket.AF_INET
with socket.socket(family,socket.SOCK_STREAM) as s:
 s.settimeout(10)
 if len(sys.argv)>2:
  output=subprocess.check_output(["/sbin/ifconfig",sys.argv[2]],text=True)
  candidates=[line.split()[1].split("%")[0] for line in output.splitlines() if line.strip().startswith(("inet ","inet6 "))]
  source=next(a for a in candidates if ipaddress.ip_address(a).version==ipaddress.ip_address(address).version and not ipaddress.ip_address(a).is_link_local)
  s.bind((source,0))
 s.connect((address,443))
print("PASS family-specific external TCP "+address+":443")' 
export VMNET_TEST_FAMILY=dual
# Configuration and unavailable attachment must fail before workload startup.
if "$vmctl" create --network team --bridge-interface en0 --name bridge-invalid "${IMAGE:-python:3.13-alpine}" true > "$work/conflicting-attachment.log" 2>&1; then
  echo 'Conflicting attachment selectors unexpectedly accepted' >&2; exit 1
fi
cat "$work/conflicting-attachment.log"
if "$vmctl" run -d --bridge-interface zzclanker0 --name bridge-unavailable "${IMAGE:-python:3.13-alpine}" python3 -c 'print("WORKLOAD-MUST-NOT-RUN")' > "$work/unavailable.log" 2>&1; then
  echo 'Unavailable physical interface unexpectedly started' >&2; exit 1
fi
cat "$work/unavailable.log"
"$vmctl" inspect bridge-unavailable | python3 -c 'import json,sys; m=json.load(sys.stdin)[0]; assert m["state"]["status"]=="created"; assert m["network"] is None'
[ -z "$(find "$work/state" -name net.pid -print)" ]
[ -z "$(find "$work/run" -name '*.net' -print)" ]
"$vmctl" rm bridge-unavailable
while IFS= read -r interface; do
  echo "Testing explicitly selected physical LAN interface: $interface"
  /sbin/ifconfig "$interface"
  has_ipv4=0
  for family in dual ipv6 ipv4; do
    if [ "$family" = ipv4 ] && [ "$has_ipv4" = 0 ]; then
      echo "LAN LIMITATION $interface: no IPv4 configured; IPv4-only packet test outstanding"
      continue
    fi
    export VMNET_TEST_FAMILY=$family
    "$vmctl" run -d --bridge-interface "$interface" --name bridge-smoke "${IMAGE:-python:3.13-alpine}" python3 -c "$services" server
    "$vmctl" exec bridge-smoke sh -c '! command -v nft'
    passed=0
    for attempt in $(seq 1 20); do
      "$vmctl" inspect bridge-smoke > "$work/$interface-$family.json"
      if python3 "$root/scripts/fixtures/vmnet-bridge-probe.py" "$work/$interface-$family.json" "$interface" "$family" > "$work/$interface-$family-probe.log" 2>&1; then passed=1; break; fi
      sleep 1
    done
    cat "$work/$interface-$family-probe.log"
    [ "$passed" = 1 ]
    if [ "$family" = dual ]; then
      for destination in 1.1.1.1 2606:4700:4700::1111; do
        if python3 -c "$external_probe" "$destination" "$interface"; then
          # Local link-local readiness can precede DHCP and router discovery.
          external_passed=0
          external_deadline=$((SECONDS + 60))
          while [ "$SECONDS" -lt "$external_deadline" ]; do
            if "$vmctl" exec bridge-smoke python3 -c "$external_probe" "$destination" > "$work/$interface-external-$destination.log" 2>&1; then
              external_passed=1
              break
            fi
            sleep 1
          done
          cat "$work/$interface-external-$destination.log"
          [ "$external_passed" = 1 ]
        else
          echo "HOST/LAN LIMITATION: external TCP $destination:443 unavailable from Mac; corresponding LAN gateway acceptance outstanding"
        fi
      done
      "$vmctl" inspect bridge-smoke > "$work/$interface-$family.json"
      python3 "$root/scripts/fixtures/vmnet-bridge-probe.py" "$work/$interface-$family.json" "$interface" "$family"
      has_ipv4=$(python3 - "$work/$interface-$family.json" <<'PY'
import ipaddress,json,sys
m=json.load(open(sys.argv[1]))[0]
print(int(any(ipaddress.ip_address(a).version==4 for a in m['network']['addresses'])))
PY
)
    fi
    "$vmctl" stop bridge-smoke
    [ -z "$(find "$work/run" -name '*.net' -print)" ]
    [ -z "$(find "$work/state" -name net.pid -print)" ]
    if [ "$family" = dual ]; then
      "$vmctl" start bridge-smoke
      "$vmctl" exec bridge-smoke python3 -c 'print("PASS vsock management after bridged restart")'
      "$vmctl" inspect bridge-smoke | python3 -c 'import json,sys; m=json.load(sys.stdin)[0]; assert m["host_config"]["bridge_interface"]==sys.argv[1]' "$interface"
      "$vmctl" stop bridge-smoke
    fi
    "$vmctl" rm bridge-smoke
  done
done < "$work/active"
echo 'PASS selected physical attachments, available family TCP/UDP, single-family startup, restart and unavailable-interface cleanup'
echo 'OUTSTANDING independent physical-LAN peer/mDNS, other attachment types not present, and external destinations unavailable from the Mac'
