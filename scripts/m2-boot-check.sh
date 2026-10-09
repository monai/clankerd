#!/usr/bin/env bash
# Real boots on the M2 (tickets 03, 05 and 09). Run on the Mac after `make rust`.
# See docs/libkrun-first-boot.md. Exits non-zero on the first failed check.
#
#   scripts/m2-boot-check.sh [boot|image|volume|net|all]   (default: all)
#   OUT=build/rust IMAGE=ghcr.io/monai/clankers:slim scripts/m2-boot-check.sh
#
# boot  (ticket 03): the guest's root is only the boot directory
#       (clankerd-guestd + workload.json), so the "command" is guestd itself,
#       run with an unknown flag: it prints an error and exits 2. That proves
#       kernel boot, guestd as PID 1, console output and the exit-code round
#       trip over vsock, without needing an image.
# image (ticket 05): boots IMAGE as the machine's root disk (the first run
#       builds the base disk with a population boot, which takes a while) and
#       checks workload user/workdir/env, /.clankerdenv, cgroup delegation,
#       exit codes, graceful stop, kill fallback and the cached second create.
# volume (ticket 09): a named volume (sparse raw ext4, formatted by guestd on
#       first use, mounted at /storage), persistence across stop/start and
#       `rm` without -v, growth at the next start, `rm -v`, and a virtio-fs
#       bind mount that is readable and writable from both sides.
# net   (ticket 08): boots IMAGE with its gvproxy sidecar and checks DHCP on
#       eth0, DNS, an HTTPS fetch, the forwarding sysctls, that gvproxy's
#       control API (192.168.127.1:80) and the host-loopback alias
#       (192.168.127.254) are unreachable, and that gvproxy ends with the
#       machine. Needs network access on the Mac (first run downloads gvproxy).
set -euo pipefail

export DYLD_FALLBACK_LIBRARY_PATH=${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}

root=$(cd "$(dirname "$0")/.." && pwd)
mode=${1:-all}
out=${OUT:-$root/build/rust}
image=${IMAGE:-ghcr.io/monai/clankers:slim}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
state=$(mktemp -d /tmp/clankerd-boot-check.XXXXXX)
export CLANKERD_STATE_DIR=$state/state
export CLANKERD_RUNTIME_DIR=$state/run
cleanup() {
  local status=$?
  if [ "$status" -ne 0 ]; then
    for m in net dev probe override stubborn second vol1 vol2 binds; do
      "$vmctl" kill "$m" >/dev/null 2>&1 || true
    done
    for machine_dir in "$CLANKERD_STATE_DIR"/machines/*; do
      [ -d "$machine_dir" ] || continue
      for log_name in vmspawn-exit vmspawn.log console.log gvproxy.log; do
        [ -f "$machine_dir/$log_name" ] || continue
        printf '\n== %s\n' "$machine_dir/$log_name" >&2
        tail -n 60 "$machine_dir/$log_name" >&2
      done
    done
    printf '\nFailed check: diagnostics preserved in %s\n' "$state" >&2
    return
  fi
  for m in net dev probe override stubborn second vol1 vol2 binds; do
    "$vmctl" rm -f "$m" >/dev/null 2>&1 || true
  done
  "$vmctl" volume rm data >/dev/null 2>&1 || true
  rm -rf "$state"
}
trap cleanup EXIT

step() { printf '\n== %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

case "$mode" in boot | image | volume | net | all) ;; *) fail "usage: $0 [boot|image|volume|net|all]" ;; esac

step "prerequisites"
[ "$(uname -sm)" = "Darwin arm64" ] || fail "run this on the Apple Silicon Mac"
for f in /opt/homebrew/lib/libkrun.1.dylib /opt/homebrew/lib/libkrunfw.5.dylib; do
  [ -e "$f" ] || fail "$f missing: brew tap slp/krun && brew install libkrun (pulls in libkrunfw)"
done
for f in "$vmctl" "$CLANKERD_VMSPAWN" "$CLANKERD_GUESTD"; do
  [ -x "$f" ] || fail "$f missing: run make rust"
done
codesign -d --entitlements - "$CLANKERD_VMSPAWN" 2>&1 | grep -q com.apple.security.hypervisor \
  || fail "clankerd-vmspawn lacks the hypervisor entitlement: run make rust-sign"
echo ok

boot_check() {
  step "boot: exit code 2 must round-trip from the guest (boot directory as root)"
  set +e
  CLANKERD_BOOT_DIR_ROOT=true "$vmctl" run --name boot-check boot-dir /clankerd-guestd --nope
  local code=$?
  set -e
  echo "vmctl exit code: $code"
  if [ "$code" -ne 2 ]; then
    echo "--- vmspawn.log" >&2
    cat "$CLANKERD_STATE_DIR"/machines/*/vmspawn.log >&2 || true
    echo "--- console.log" >&2
    CLANKERD_BOOT_DIR_ROOT=true "$vmctl" logs boot-check >&2 || true
    fail "expected exit code 2, got $code"
  fi

  step "logs: kernel and guestd console output"
  local logs
  logs=$(CLANKERD_BOOT_DIR_ROOT=true "$vmctl" logs boot-check)
  printf '%s\n' "$logs" | head -n 40
  printf '%s\n' "$logs" | grep -qi 'linux version' || echo "WARN: no kernel banner in logs (kernel may boot quietly)"
  printf '%s\n' "$logs" | grep -q 'unknown argument --nope' || fail "no guestd output in logs"
  CLANKERD_BOOT_DIR_ROOT=true "$vmctl" rm boot-check >/dev/null
}

# --- ticket 05 helpers -------------------------------------------------------

# Value of the first "key": "value" in `vmctl inspect NAME` (config comes first).
inspect_field() { "$vmctl" inspect "$1" | sed -n "s/.*\"$2\": \"\\(.*\\)\".*/\\1/p" | head -n 1; }

expect_status() { # name, text the STATUS column must contain
  local s
  s=$("$vmctl" ps -a | grep -E "[[:space:]]$1\$" || true)
  case "$s" in *"$2"*) echo "ok   $1: $2" ;; *)
    echo "--- $1 console" >&2
    "$vmctl" logs "$1" | tail -n 30 >&2 || true
    fail "$1 should be '$2', ps says: $s" ;;
  esac
}

logs_have() { # name, fixed string
  "$vmctl" logs "$1" | grep -qF -- "$2" || {
    "$vmctl" logs "$1" | tail -n 40 >&2
    fail "$1 console lacks: $2"
  }
  echo "ok   $1 console has: $2"
}

wait_exec() { # name: until exec works (the machine is booted and guestd serves)
  for _ in $(seq 1 60); do
    "$vmctl" exec "$1" true >/dev/null 2>&1 && return 0
    sleep 1
  done
  "$vmctl" logs "$1" | tail -n 40 >&2
  fail "exec never became available in $1"
}

seconds() { date +%s; }

image_check() {
  [ -x "$out/linux-arm64/mke2fs" ] || fail "$out/linux-arm64/mke2fs missing: run make rust (it builds e2fsprogs)"

  step "pull $image"
  "$vmctl" pull "$image"

  step "run -d: boots from the image (the first run builds the base disk, minutes)"
  "$vmctl" run -d --name dev "$image" sleep infinity
  wait_exec dev
  expect_status dev "Up"

  step "image USER, WORKDIR and env reach the machine"
  want_user=$(inspect_field dev user)
  want_dir=$(inspect_field dev working_dir)
  echo "image user='${want_user}' workdir='${want_dir}'"
  "$vmctl" exec dev sh -c 'echo "user=$(id -un) pwd=$(pwd)"; env | sort'
  if [ -n "$want_dir" ]; then
    [ "$("$vmctl" exec dev pwd)" = "$want_dir" ] || fail "exec pwd is not the image WORKDIR $want_dir"
  fi
  case "$want_user" in
    '' | *[0-9]*) echo "(numeric or empty image USER: compare by hand in the output above)" ;;
    *) [ "$("$vmctl" exec dev id -un)" = "$want_user" ] || fail "exec user is not the image USER $want_user" ;;
  esac
  
  "$vmctl" run --name probe --entrypoint sh "$image" -c 'echo "workload-user=$(id -un) workload-pwd=$(pwd)"' || fail "probe run failed"
  logs_have probe "workload-pwd=${want_dir:-/}"
  case "$want_user" in '' | *[0-9]*) ;; *) logs_have probe "workload-user=$want_user" ;; esac

  step "/.clankerdenv and cgroup v2 delegation"
  "$vmctl" exec dev test -e /.clankerdenv || fail "/.clankerdenv missing"
  controllers=$("$vmctl" exec dev cat /sys/fs/cgroup/cgroup.subtree_control)
  echo "subtree_control: $controllers"
  [ -n "$controllers" ] || fail "no cgroup v2 controllers delegated"
  echo "guest date: $("$vmctl" exec dev date -u +%s)  host date: $(date -u +%s)"

  step "stop is graceful: shutdown steps in the console, exit 143 for a plain sleep"
  "$vmctl" stop dev
  expect_status dev "Exited (143)"
  logs_have dev "syncing and unmounting"
  logs_have dev "powering off"

  step "run IMAGE true exits with the workload"
  "$vmctl" run --name second "$image" true && code=0 || code=$?
  [ "$code" -eq 0 ] || fail "vmctl run IMAGE true exited $code"
  expect_status second "Exited (0)"

  step "-e, -u and -w override the image"
  "$vmctl" run --name override -e FOO=bar -u root -w /tmp --entrypoint sh "$image" \
    -c 'echo "o=$(id -un) $(pwd) $FOO"'
  logs_have override "o=root /tmp bar"

  step "stop falls back to kill after the timeout"
  "$vmctl" run -d --name stubborn --entrypoint sh "$image" -c 'trap "" TERM; sleep infinity'
  wait_exec stubborn
  began=$(seconds)
  "$vmctl" stop -t 2 stubborn
  took=$(($(seconds) - began))
  echo "stop -t 2 took ${took}s"
  [ "$took" -ge 2 ] && [ "$took" -le 15 ] || fail "stop -t 2 took ${took}s"
  expect_status stubborn "Exited (137)"

  step "a second machine from the cached image is created almost instantly"
  "$vmctl" rm -f second >/dev/null
  began=$(seconds)
  "$vmctl" create --name second "$image" true >/dev/null
  took=$(($(seconds) - began))
  echo "create from the cached image took ${took}s"
  [ "$took" -le 10 ] || fail "second create took ${took}s (is the base disk cloned?)"
}

expect_volume_capacity() {
  local machine=$1 expected=$2 logical sectors
  logical=$("$vmctl" volume inspect data | sed -n 's/.*"size": \([0-9]*\).*/\1/p')
  case "$logical" in '' | *[!0-9]*) fail "volume inspect did not return a numeric capacity" ;; esac
  [ "$logical" -eq "$expected" ] || fail "volume capacity is $logical, expected $expected"
  sectors=$("$vmctl" exec "$machine" cat /sys/class/block/vdb/size)
  case "$sectors" in '' | *[!0-9]*) fail "guest did not return a numeric block-device capacity" ;; esac
  [ "$sectors" -eq "$((expected / 512))" ] \
    || fail "guest volume device has $sectors sectors, expected $((expected / 512))"
  echo "volume capacity: $logical bytes; guest device: $sectors sectors"
  echo "backing file EOF: $(stat -f %z "$volfile"); host allocation: $(du -h "$volfile" | cut -f1)"
}

volume_check() {
  [ -x "$out/linux-arm64/mke2fs" ] && [ -x "$out/linux-arm64/resize2fs" ] \
    || fail "mke2fs/resize2fs missing in $out/linux-arm64: run make rust (it builds e2fsprogs)"

  "$vmctl" pull "$image" >/dev/null

  step "new volume: sparse on the host, formatted and mounted by guestd"
  "$vmctl" run -d --name vol1 -v data:/storage --volume-size 1G "$image" sleep infinity
  wait_exec vol1
  volfile=$CLANKERD_STATE_DIR/volumes/data/data.ext4
  expect_volume_capacity vol1 1073741824
  "$vmctl" exec vol1 sh -c 'grep " /storage " /proc/mounts; df -h /storage'
  "$vmctl" exec vol1 sh -c 'grep " /storage " /proc/mounts | grep -q ext4' || fail "/storage is not an ext4 mount"
  logs_have vol1 "formatted new volume"
  "$vmctl" exec -u root vol1 sh -c 'echo persisted > /storage/marker && sync'

  step "the data survives stop/start (no reformat)"
  "$vmctl" stop vol1
  "$vmctl" start vol1
  wait_exec vol1
  expect_volume_capacity vol1 1073741824
  [ "$("$vmctl" exec vol1 cat /storage/marker)" = persisted ] || fail "marker lost across stop/start"

  step "the data survives rm without -v, and a new machine reuses the volume"
  "$vmctl" stop vol1
  "$vmctl" rm vol1
  "$vmctl" volume ls | grep -q '^data ' || fail "volume was removed by plain rm"

  step "a larger size takes effect at the next start (resize2fs)"
  "$vmctl" run -d --name vol2 -v data:/storage:size=2G "$image" sleep infinity
  wait_exec vol2
  expect_volume_capacity vol2 2147483648
  logs_have vol2 "grew volume"
  total=$("$vmctl" exec vol2 sh -c 'stat -f -c "%b * %S" /storage' | bc)
  echo "filesystem bytes: $total"
  [ "$total" -gt 1900000000 ] || fail "filesystem did not grow (total $total)"
  [ "$("$vmctl" exec vol2 cat /storage/marker)" = persisted ] || fail "marker lost after growing"

  step "rm -v deletes the volume"
  "$vmctl" rm -f -v vol2
  [ ! -e "$volfile" ] || fail "volume file still exists after rm -v"

  step "bind mount: readable and writable from both sides"
  hostdir=$state/shared
  mkdir -p "$hostdir"
  echo from-host > "$hostdir/host.txt"
  "$vmctl" run -d --name binds -v "$hostdir":/shared "$image" sleep infinity
  wait_exec binds
  [ "$("$vmctl" exec binds cat /shared/host.txt)" = from-host ] || fail "guest cannot read the host file"
  "$vmctl" exec -u root binds sh -c 'echo from-guest > /shared/guest.txt && sync'
  [ "$(cat "$hostdir/guest.txt")" = from-guest ] || fail "host cannot read the guest's file"
  echo host-update > "$hostdir/host.txt"
  [ "$("$vmctl" exec binds cat /shared/host.txt)" = host-update ] || fail "guest does not see the host's update"
  "$vmctl" exec binds sh -c 'ls -ln /shared; id -u'
  "$vmctl" rm -f binds
}

net_check() {
  step "net: boot IMAGE with a gvproxy sidecar"
  "$vmctl" run -d --name net --entrypoint sleep "$image" infinity
  wait_exec net

  step "net: eth0 got 192.168.127.2 by DHCP"
  "$vmctl" exec net sh -c 'ip -4 addr show eth0 || ifconfig eth0' | tee /dev/stderr | grep -q 192.168.127.2 \
    || fail "eth0 has no DHCP address"

  step "net: DNS resolves and HTTPS works"
  "$vmctl" exec net cat /etc/resolv.conf
  "$vmctl" exec net sh -c 'getent hosts example.com || nslookup example.com' || fail "DNS lookup failed"
  "$vmctl" exec net sh -c '
    body=$(
      if command -v curl >/dev/null 2>&1; then
        curl -fsS --max-time 15 https://example.com
      elif command -v wget >/dev/null 2>&1; then
        wget -T 15 -qO- https://example.com
      else
        echo "curl or wget is required for the HTTPS check" >&2
        exit 127
      fi
    ) || exit 1
    [ -n "$body" ] || exit 1
    printf "%s\n" "$body" | head -c 200
  ' \
    || fail "HTTPS fetch failed"

  step "net: forwarding sysctls are set"
  [ "$("$vmctl" exec net cat /proc/sys/net/ipv4/ip_forward)" = 1 ] || fail "net.ipv4.ip_forward is not 1"

  step "net: gvproxy's control API and the host-loopback alias are unreachable"
  if "$vmctl" exec net sh -c 'wget -T 3 -qO- http://192.168.127.1/services/forwarder/all || curl -m 3 -fsS http://192.168.127.1/services/forwarder/all'; then
    fail "the guest reached gvproxy's control API"
  fi
  if "$vmctl" exec net sh -c 'wget -T 3 -qO- http://192.168.127.254:22 || curl -m 3 -sS http://192.168.127.254:22'; then
    fail "the guest reached the host loopback alias"
  fi

  step "net: gvproxy stops with the machine"
  pgrep -f "gvproxy.*$state" >/dev/null || fail "no gvproxy sidecar is running"
  "$vmctl" rm -f net >/dev/null
  ! pgrep -f "gvproxy.*$state" >/dev/null || fail "gvproxy outlived its machine"
}

case "$mode" in
  boot) boot_check ;;
  image) image_check ;;
  volume) volume_check ;;
  net) net_check ;;
  all) boot_check; image_check; volume_check; net_check ;;
esac

step "done"
echo "PASS ($mode). Still by hand: docs/libkrun-first-boot.md section 4 (error messages) and the unverified list in sections 5 and 7."
