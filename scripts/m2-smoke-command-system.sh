#!/usr/bin/env bash
# Included in m2-run-network-gates.sh: stopped edits, reimage and data retention.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
work=$(mktemp -d /tmp/clankerd-command-system.XXXXXX)
export CLANKERD_STATE_DIR=$work/state CLANKERD_RUNTIME_DIR=$work/run
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
image=${IMAGE:-python:3.13-alpine}
cleanup() { "$vmctl" machine rm -f member >/dev/null 2>&1 || true; }
trap cleanup EXIT
trap 'echo "FAIL line $LINENO: $BASH_COMMAND; evidence: $work" >&2' ERR
mkdir "$work/first" "$work/second"
echo retained > "$work/second/host-marker"
"$vmctl" network create first
"$vmctl" network create --mode host-only second
"$vmctl" volume create --size 64M data
"$vmctl" machine create --name member --network first --cpus 1 --memory 256M \
  --mount type=volume,source=data,target=/storage \
  --mount "type=bind,source=$work/first,target=/shared" \
  "$image" sleep infinity
"$vmctl" machine update --cpus 2 --memory 512M --network second \
  --mount-remove /shared --mount-add "type=bind,source=$work/second,target=/shared" \
  --socket-add "host=$work/host.sock,guest=/run/host.sock" member
"$vmctl" machine inspect member > "$work/updated.json"
python3 - "$work/updated.json" <<'PY'
import json,sys
m=json.load(open(sys.argv[1]))[0]; h=m['host_config']
assert h['cpus']==2 and h['memory']==512*1024*1024
assert h['virtual_network']=='second' and h['network_mode']=='host-only'
assert h['socket_bindings'][0]['guest_path']=='/run/host.sock'
assert len(h['mounts'])==2 and m['config']['cmd']==['sleep','infinity']
assert m['state']['status']=='created'
PY
"$vmctl" machine update --socket-remove /run/host.sock member
"$vmctl" machine start member
"$vmctl" machine exec member sh -c '[ "$(cat /shared/host-marker)" = retained; echo persisted > /storage/marker; sync'
if "$vmctl" machine update --memory 1G member; then echo 'running update accepted' >&2; exit 1; fi
"$vmctl" machine stop member
"$vmctl" machine update --network first member
"$vmctl" machine reimage member "${ALT_IMAGE:-alpine:3.23}"
"$vmctl" machine start member
"$vmctl" machine exec member sh -c '[ "$(cat /storage/marker)" = persisted ] && [ "$(cat /shared/host-marker)" = retained ]'
"$vmctl" machine stop member
"$vmctl" machine update --mount-remove /storage member
"$vmctl" machine rm member
"$vmctl" volume inspect data > "$work/retained.json"
python3 - "$work/retained.json" <<'PY'
import json,sys
v=json.load(open(sys.argv[1]))[0]
assert v['name']=='data' and v['size']==64*1024*1024
PY
"$vmctl" volume rm data
"$vmctl" network rm first second
echo "PASS command-system acceptance; evidence: $work"
