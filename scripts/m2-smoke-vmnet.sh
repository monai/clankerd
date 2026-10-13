#!/usr/bin/env bash
# Real macOS 15 shared-network gate. Run after `mise exec -- make rust`.
# Requires vmnet-helper installed with its supported passwordless sudoers rule.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" = 15 ] || { echo 'This gate requires macOS 15' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
export CLANKERD_VMNET_HELPER=${CLANKERD_VMNET_HELPER:-/opt/vmnet-helper/bin/vmnet-helper}
sudo --non-interactive -- "$CLANKERD_VMNET_HELPER" --version
work=$(mktemp -d /tmp/clankerd-vmnet.XXXXXX)
export CLANKERD_STATE_DIR=$work/state
export CLANKERD_RUNTIME_DIR=$work/run
cleanup() {
  "$vmctl" rm -f vmnet-smoke >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
trap 'echo "FAIL line $LINENO: $BASH_COMMAND" >&2; find "$work/state" -name "*.log" -exec tail -40 {} \;' ERR
# Python image supplies service fixtures but no nft. Both TCP and UDP listeners
# bind directly to the guest NIC and continue after the initiating CLI exits.
server='import socket,threading,http.server
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("0.0.0.0",18082))
def udp():
 while True:
  data,peer=s.recvfrom(2048);s.sendto(data,peer)
threading.Thread(target=udp,daemon=True).start()
http.server.HTTPServer(("0.0.0.0",18081),http.server.SimpleHTTPRequestHandler).serve_forever()'
"$vmctl" run -d --name vmnet-smoke "${IMAGE:-python:3.13-alpine}" python3 -c "$server"
address() {
  "$vmctl" inspect vmnet-smoke | python3 -c 'import json,sys,ipaddress; m=json.load(sys.stdin)[0]; assert m["host_config"]["network_mode"]=="shared"; print(next(a for a in m["network"]["addresses"] if ipaddress.ip_address(a).version==4))'
}
services() {
  local ip=$1
  curl --fail --silent --max-time 10 "http://$ip:18081/" >/dev/null
  python3 - "$ip" <<'PY'
import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(10)
s.sendto(b'vmnet-udp', (sys.argv[1],18082))
assert s.recvfrom(2048)[0] == b'vmnet-udp'
PY
}
ip=$(address)
echo "guest IPv4: $ip"
for attempt in $(seq 1 20); do services "$ip" && break; sleep 1; done
services "$ip"
"$vmctl" exec vmnet-smoke sh -c '! command -v nft && python3 -c '\''import urllib.request; print(urllib.request.urlopen("https://example.com",timeout=20).status)'\'''
"$vmctl" stop vmnet-smoke
[ -z "$(find "$work/run" -name '*.net' -print)" ]
[ -z "$(find "$work/state" -name net.pid -print)" ]
"$vmctl" start vmnet-smoke
ip=$(address)
for attempt in $(seq 1 20); do services "$ip" && break; sleep 1; done
services "$ip"
"$vmctl" stop vmnet-smoke
[ -z "$(find "$work/run" -name '*.net' -print)" ]
[ -z "$(find "$work/state" -name net.pid -print)" ]
echo 'PASS macOS 15: IPv4 TCP/UDP, outbound NAT, no nft, detached lifetime, restart and cleanup'
