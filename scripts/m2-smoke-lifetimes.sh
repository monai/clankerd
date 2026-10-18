#!/usr/bin/env bash
# macOS gate for actual dhcpcd lease/RA maintenance in a running VM.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
work=$(mktemp -d /tmp/clankerd-lifetimes.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
export VMNET_LIFETIME_CONTROL=$work
export VMNET_TEST_REAL_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_VMSPAWN=$root/scripts/fixtures/vmnet-lifetimes.py
export CLANKERD_GUEST_ROOT=$out/linux-arm64/management
echo "Mac inherited open-file limits soft=$(ulimit -Sn) hard=$(ulimit -Hn)"
name=vmnet-lifetimes
failed=0
cleanup() {
  "$vmctl" rm -f "$name" >/dev/null 2>&1 || true
  if [ "$failed" = 1 ]; then echo "Retained diagnostics: $work"; else rm -rf "$work"; fi
}
sample_native() {
  phase=$1
  if [ -f "$work/native.pid" ]; then
    native_pid=$(cat "$work/native.pid")
    echo "FD sample $phase native pid=$native_pid"
    cat "$work/native-limits"
    lsof -nP -p "$native_pid" > "$work/native-fds-$phase.txt" 2>&1 || true
    awk 'NR>1 && $4 ~ /^[0-9]/ {count[$5]++; n++} END {printf "native descriptors=%d",n; for (kind in count) printf " %s=%d",kind,count[kind]; print ""}' "$work/native-fds-$phase.txt"
  fi
}
failure_diagnostics() {
  failed=1
  sample_native failure
  sleep 10 # Longer than libkrun's ordinary deferred socket reap interval.
  sample_native failure-idle
  find "$work/state" -name "*.log" -exec tail -100 {} \;
}
trap cleanup EXIT
trap failure_diagnostics ERR
python3 "$CLANKERD_VMSPAWN" --self-test
services=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
"$vmctl" run -d --name "$name" "${IMAGE:-python:3.13-alpine}" python3 -c "$services" server
check() {
  "$vmctl" inspect "$name" > "$work/inspect.json"
  python3 - "$work/inspect.json" "$1" "$work/events" <<'PY'
import json, sys
from pathlib import Path
inspection = json.load(open(sys.argv[1]))
# Network status is found without coupling to machine inspect metadata.
def find(value):
    if isinstance(value, dict):
        if 'address_details' in value and 'ipv4' in value: return value
        for child in value.values():
            result = find(child)
            if result is not None: return result
    if isinstance(value, list):
        for child in value:
            result = find(child)
            if result is not None: return result
status = find(inspection)
assert status is not None, inspection
phase = sys.argv[2]
events = Path(sys.argv[3]).read_text().splitlines()
prefix = 'fd73:736d:6f6b:7:'
pio = [a for a in status['address_details'] if a['address'].startswith(prefix)]
servers = status['nameservers']
if phase == 'initial':
    assert any(not a['tentative'] and not a['deprecated'] for a in pio), status
    assert 'fd73:736d:6f6b:7::53' in servers, status
    assert '192.0.2.53' in servers, status
    assert 'ra-lifetime.test' in status['search'] and 'lifetime.test' in status['search'], status
    assert status['ipv4']['local_available'], status
    assert any(r['gateway'] == 'fe80::1234' and r['prefix_len'] == 0 for r in status['routes']), status
elif phase == 'renewed':
    assert events.count('ack') >= 2 and 'renew' in events, events
    assert '192.0.2.54' in servers and '192.0.2.53' not in servers, status
    assert 'fd73:736d:6f6b:7::53' in servers, status
elif phase == 'renewed6':
    assert events.count('reply6') >= 2 and 'renew6' in events, events
    assert any(a['address'] == 'fd73:736d:6f6b:8::99' and not a['tentative'] for a in status['address_details']), status
    assert '2001:db8::54' in servers and '2001:db8::53' not in servers, status
elif phase == 'deprecated':
    assert any(a['deprecated'] and not a['dad_failed'] for a in pio), status
    assert status['ipv4']['local_available'], status
elif phase == 'ra-expired':
    assert not pio, status
    assert 'fd73:736d:6f6b:7::53' not in servers, status
    assert 'ra-lifetime.test' not in status['search'] and 'lifetime.test' in status['search'], status
    assert not any(r['gateway'] == 'fe80::1234' for r in status['routes']), status
    assert '192.0.2.54' in servers and status['ipv4']['local_available'], status
elif phase == 'nak-reacquired':
    assert 'nak' in events and events.count('ack') >= 3, events
    assert status['ipv4']['local_available'] and '192.0.2.54' in servers, status
elif phase == 'dhcp-expired':
    assert 'rebind' in events, events
    assert not any('.' in a['address'] for a in status['address_details']), status
    assert not status['ipv4']['default_route'] and not status['ipv4']['local_available'], status
    assert '192.0.2.54' not in servers and 'lifetime.test' not in status['search'], status
    assert status['ipv6']['local_available'] and '2001:db8::54' in servers, status
elif phase == 'deprecated6':
    assert any(a['address'] == 'fd73:736d:6f6b:8::99' and a['deprecated'] for a in status['address_details']), status
elif phase == 'dhcp6-expired':
    assert 'rebind6' in events, events
    assert not any(a['address'] == 'fd73:736d:6f6b:8::99' for a in status['address_details']), status
    assert '2001:db8::54' not in servers, status
print('PASS', phase)
PY
}
wait_for() {
  for attempt in $(seq 1 "$2"); do
    if check "$1" > "$work/check.log" 2>&1; then cat "$work/check.log"; sample_native "$1"; return; fi
    sleep 1
  done
  cat "$work/check.log" >&2
  return 1
}
# Initial observation must precede T1, rather than silently skip the old DNS.
wait_for initial 6
"$vmctl" exec "$name" python3 -c 'import os; print("guestd descriptors", len(os.listdir("/proc/1/fd")), "threads", len(os.listdir("/proc/1/task")))'
wait_for renewed 30
wait_for renewed6 30
"$vmctl" inspect "$name" > "$work/connected.json"
python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/connected.json" ipv4-services
touch "$work/stop-ra"
wait_for deprecated 12
wait_for ra-expired 20
"$vmctl" exec "$name" python3 -c 'import os; print("guestd descriptors", len(os.listdir("/proc/1/fd")), "threads", len(os.listdir("/proc/1/task")))'
touch "$work/nak"
wait_for nak-reacquired 35
"$vmctl" inspect "$name" > "$work/connected.json"
python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/connected.json" ipv4-services
touch "$work/drop-dhcp"
wait_for dhcp-expired 40
touch "$work/drop-dhcp6"
wait_for deprecated6 24
wait_for dhcp6-expired 24
"$vmctl" exec "$name" python3 -c 'print("PASS workload and vsock survive lease expiry")'
"$vmctl" stop "$name"
"$vmctl" rm "$name"
echo 'PASS real VM DHCP renewal/changed DNS/NAK/rebinding/expiry and fresh-prefix RA deprecation/address/router/DNS expiry plus DHCPv6 renew/changed DNS/rebind/address and DNS expiry'
