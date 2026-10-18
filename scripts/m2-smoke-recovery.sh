#!/usr/bin/env bash
# Real macOS 15 recovery without changing the workload or its selected network.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" = 15 ] || { echo 'Requires macOS 15' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_NET_DIAGNOSTICS=1
export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
work=$(mktemp -d /tmp/clankerd-recovery.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
name=vmnet-recovery
failed=0
native_pid=''
fd_baseline=''
host_pid=''
management_round=0
cleanup() {
  "$vmctl" rm -f "$name" >/dev/null 2>&1 || true
  if [ -n "$host_pid" ]; then kill "$host_pid" 2>/dev/null || true; wait "$host_pid" 2>/dev/null || true; fi
  if [ "$failed" = 1 ]; then echo "Retained diagnostics: $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT
failure_diagnostics() {
  failed=1
  if [ -n "$native_pid" ]; then fd_sample failure || true; sleep 6; fd_sample failure-idle || true; fi
  python3 - "$work" <<'PY'
from pathlib import Path
import shutil, sys
work = Path(sys.argv[1])
state = work / 'state'
for source in state.rglob('*.log'):
    target = work / 'logs' / source.relative_to(state)
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)
PY
  "$root/scripts/m2-report-recovery.sh" "$work" || true
}
fd_sample() {
  local tag=$1
  kill -0 "$native_pid" || { echo "VMM exited before descriptor sample $tag"; return 1; }
  lsof -nP -p "$native_pid" > "$work/native-fds-$tag.txt"
  descriptors=$(awk 'NR>1 && $4 ~ /^[0-9]/ {n++} END {print n+0}' "$work/native-fds-$tag.txt")
  echo "$descriptors" > "$work/native-count-$tag"
  echo "FD sample $tag: native pid=$native_pid descriptors=$descriptors"
  awk 'NR>1 && $4 ~ /^[0-9]/ {count[$5]++} END {for(kind in count) printf "%s=%d ",kind,count[kind]; print ""}' "$work/native-fds-$tag.txt"
  # Retain exact Unix node identities and endpoints, not only aggregate counts.
  awk 'NR==1 || ($4 ~ /^[0-9]/ && $5=="unix")' "$work/native-fds-$tag.txt" > "$work/native-unix-$tag.txt"
  lsof -nP -a -p "$native_pid" -U -FfnT > "$work/native-unix-fields-$tag.txt"
}
management_sample() {
  local tag="management-$management_round-$1"
  fd_sample "$tag-busy"
  sleep 6
  fd_sample "$tag-idle"
  "$vmctl" exec "$name" python3 -c 'import os;print("guestd descriptors/threads",len(os.listdir("/proc/1/fd")),len(os.listdir("/proc/1/task")))' > "$work/guest-$tag.txt"
  cat "$work/guest-$tag.txt"
}
fd_idle() {
  sleep 6 # libkrun normally defers proxy reclamation for five seconds.
  fd_sample "$1"
  [ "$descriptors" -le "$((fd_baseline + 32))" ] || {
    echo "FAIL descriptor growth after idle: baseline=$fd_baseline current=$descriptors" >&2
    return 1
  }
}
trap failure_diagnostics ERR
fixture=$root/scripts/fixtures/vmnet-recovery-services.py
python3 "$root/scripts/fixtures/vmnet-recovery-unix-self-test.py"
python3 "$fixture" unix-server "$work/host.sock" &
host_pid=$!
services=$(cat "$root/scripts/fixtures/vmnet-ipv6-services.py")
recovery=$(cat "$fixture")
id=$("$vmctl" run -d -i --name "$name" "${IMAGE:-python:3.13-alpine}" python3 -c "$services
$recovery" recovery-server)
echo "$id"
short=${id:0:12}
dir=$work/state/machines/$id
workload_pid=''
startup_deadline=$((SECONDS + 30))
while [ "$SECONDS" -lt "$startup_deadline" ]; do
  if workload_pid=$("$vmctl" exec "$name" python3 -c "$recovery" ready 2> "$work/startup.log"); then
    break
  fi
  sleep 0.2
done
if [ -z "$workload_pid" ]; then
  cat "$work/startup.log" >&2
  echo 'Workload fixture PID/listeners did not become ready within 30 seconds' >&2
  false
fi
"$vmctl" inspect "$name" > "$work/initial.json"
native_pid=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))[0]["state"]["pid"])' "$work/initial.json")
python3 "$fixture" bind "$work/run/$short.tunnels" "$work/host.sock"
check() {
  "$vmctl" inspect "$name" > "$work/current.json"
  python3 - "$work/initial.json" "$work/current.json" "$1" <<'PY'
import json,sys
before=json.load(open(sys.argv[1]))[0]
after=json.load(open(sys.argv[2]))[0]
assert after['state']['status']=='running', after
assert after['state']['started_at']==before['state']['started_at'], 'workload restarted'
assert after['host_config']==before['host_config'], 'selected network policy changed'
network=after['network']
assert network is not None, after
if sys.argv[3]=='down':
    assert not network['ready'], network
    assert not network['ipv4']['local_available'] and not network['ipv6']['local_available'], network
else:
    assert network['ready'] and network['ipv4']['local_available'], network
print('PASS observed connectivity',sys.argv[3])
PY
}
wait_for() {
  for attempt in $(seq 1 "$2"); do
    if check "$1" > "$work/check.log" 2>&1; then cat "$work/check.log"; return; fi
    sleep 0.2
  done
  cat "$work/check.log" >&2; return 1
}
management() {
  management_round=$((management_round + 1))
  management_sample before
  [ "$("$vmctl" exec "$name" cat /tmp/recovery-workload.pid)" = "$workload_pid" ]
  "$vmctl" exec "$name" python3 -c "$recovery" guest-unix
  management_sample unix
  printf 'recovery-exec\n' | "$vmctl" exec -i "$name" python3 -c 'import sys;assert sys.stdin.read()=="recovery-exec\n";print("PASS exec stdin/output")'
  management_sample exec-stdin
  previous_signal=$("$vmctl" exec "$name" python3 -c 'from pathlib import Path;p=Path("/tmp/recovery-signal");print(p.read_text() if p.exists() else "none")')
  "$vmctl" exec "$name" kill -s USR1 "$workload_pid"
  delivered=0
  for attempt in $(seq 1 10); do
    current_signal=$("$vmctl" exec "$name" python3 -c 'from pathlib import Path;p=Path("/tmp/recovery-signal");print(p.read_text() if p.exists() else "none")')
    if [ "$current_signal" != "$previous_signal" ]; then delivered=1; break; fi
    sleep 0.1
  done
  [ "$delivered" = 1 ]
  management_sample signal
  python3 "$fixture" attach "$work/run/$short.sock"
  management_sample attach
}
packets() {
  "$vmctl" inspect "$name" > "$work/packet.json"
  python3 "$root/scripts/fixtures/vmnet-ipv6-services.py" host "$work/packet.json" ipv4-services
}
wait_for up 100
packets
fd_sample startup-busy
sleep 6
fd_sample baseline-idle
fd_baseline=$descriptors
"$vmctl" exec "$name" python3 -c 'import os;print("guestd descriptors",len(os.listdir("/proc/1/fd")))'
for attempt in $(seq 1 12); do
  "$vmctl" inspect "$name" > "$work/stress.json"
  "$vmctl" exec "$name" true
  "$vmctl" exec "$name" python3 -c "$recovery" guest-unix
done
fd_sample burst-busy
fd_idle burst-idle
"$vmctl" exec "$name" python3 -c 'import os;print("guestd descriptors",len(os.listdir("/proc/1/fd")))'
# Real link/address loss is held throughout management and Unix binding probes.
echo carrier-down > "$work/phase"
"$vmctl" exec "$name" ip link set eth0 down
"$vmctl" exec "$name" ip -4 addr flush dev eth0
wait_for down 20
management
echo carrier-up > "$work/phase"
"$vmctl" exec "$name" ip link set eth0 up
wait_for up 150
packets
# dhcpcd is independently supervised; its death must not restart the workload.
echo client-restart > "$work/phase"
"$vmctl" exec "$name" python3 -c "$recovery" kill-client
wait_for down 10
wait_for up 150
management
packets
# Detached native supervision reconnects the same VMM endpoint after helper death.
for cycle in 1 2; do
  echo "helper-restart-$cycle" > "$work/phase"
  old_helper=$(cat "$dir/net.pid")
  kill -KILL "$old_helper"
  wait_for down 10
  management
  wait_for up 150
  new_helper=$(cat "$dir/net.pid")
  [ "$old_helper" != "$new_helper" ]
  packets
  [ "$(find "$work/run" -name '*.relay' | wc -l | tr -d ' ')" = 1 ]
  [ "$(find "$work/run" -name '*.relay-peer' | wc -l | tr -d ' ')" = 1 ]
  fd_sample "recovery-$cycle-busy"
  fd_idle "recovery-$cycle-idle"
done
"$vmctl" stop "$name"
[ -z "$(find "$work/run" \( -name '*.net' -o -name '*.relay' -o -name '*.relay-peer' \) -print)" ]
[ -z "$(find "$work/state" -name net.pid -print)" ]
"$vmctl" rm "$name"
echo 'PASS real VM link/address loss, configuration-client restart, repeated native helper replacement, stable workload/vsock/Unix binding and cleanup; host sleep/wake and external IPv6 remain unverified'
