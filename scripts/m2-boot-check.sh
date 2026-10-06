#!/usr/bin/env bash
# First real boot on the M2 (ticket 03). Run on the Mac after `make rust`.
# See docs/libkrun-first-boot.md. Exits non-zero on the first failed check.
#
# The guest's root is only the boot directory (clankerd-guestd + workload.json),
# so the "command" is guestd itself, run with an unknown flag: it prints an
# error and exits 2. That proves kernel boot, guestd as PID 1, console output
# and the exit-code round trip over vsock, without needing an image yet.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/build/rust}
vmctl=$out/darwin-arm64/vmctl
export CLANKERD_VMSPAWN=$out/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$out/linux-arm64/clankerd-guestd
state=$(mktemp -d /tmp/clankerd-boot-check.XXXXXX)
export CLANKERD_STATE_DIR=$state/state
export CLANKERD_RUNTIME_DIR=$state/run
trap 'rm -rf "$state"' EXIT

step() { printf '\n== %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

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

step "boot: exit code 2 must round-trip from the guest"
set +e
"$vmctl" run --name boot-check boot-dir /clankerd-guestd --nope
code=$?
set -e
echo "vmctl exit code: $code"
if [ "$code" -ne 2 ]; then
  echo "--- vmspawn.log" >&2
  cat "$CLANKERD_STATE_DIR"/machines/*/vmspawn.log >&2 || true
  echo "--- console.log" >&2
  "$vmctl" logs boot-check >&2 || true
  fail "expected exit code 2, got $code"
fi

step "logs: kernel and guestd console output"
logs=$("$vmctl" logs boot-check)
printf '%s\n' "$logs" | head -n 40
printf '%s\n' "$logs" | grep -qi 'linux version' || echo "WARN: no kernel banner in logs (kernel may boot quietly)"
printf '%s\n' "$logs" | grep -q 'unknown argument --nope' || fail "no guestd output in logs"

step "clean error without libkrun is checked by hand (see the doc)"
echo "PASS"
