#!/usr/bin/env bash
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
resume=''
if [ "${1:-}" = --resume ]; then
  [ "$#" -ge 2 ] || { echo 'Usage: m2-run-network-gates.sh [--resume EVIDENCE_DIR] [GATE ...]' >&2; exit 1; }
  resume=$2
  shift 2
  [ -d "$resume" ] || { echo "Missing evidence directory: $resume" >&2; exit 1; }
fi
work=$(mktemp -d /tmp/clankerd-network-gates.XXXXXX)
echo "Suite evidence: $work"
sudo -n true
(
  while sleep 30; do sudo -n -v || exit; done
) &
refresh_pid=$!
trap 'kill "$refresh_pid" 2>/dev/null || true' EXIT
{
  sw_vers
  uname -m
  ls -l /opt/homebrew/lib/libkrun.1.dylib
  /usr/bin/otool -L /opt/homebrew/lib/libkrun.1.dylib
  sysctl net.inet.ip.forwarding net.inet6.ip6.forwarding
} > "$work/host-preflight.log" 2>&1 || true
stages=(m2-smoke-other-vmm m2-smoke-host-only m2-test-isolation-pf-tags m2-test-isolation-pf-state m2-smoke-vmnet m2-smoke-ipv6 m2-smoke-lifetimes m2-smoke-recovery m2-smoke-bridged m2-smoke-test)
if [ "$#" -gt 0 ]; then
  for requested in "$@"; do
    found=0
    for stage in "${stages[@]}"; do [ "$stage" != "$requested" ] || found=1; done
    [ "$found" = 1 ] || { echo "Unknown gate: $requested" >&2; exit 1; }
  done
  stages=("$@")
fi
# Reuse only successful checkpoints with identical code, binaries and policy.
# Older logs without fingerprints remain evidence, but cannot authorize a skip.
fingerprint() {
  {
    printf '%s\n' "$1" "${PF_TEST_STATE_POLICY:-if-bound}" "${IMAGE:-}" "${OUT:-}"
    git -C "$root" rev-parse HEAD
    git -C "$root" diff HEAD -- crates scripts Makefile Cargo.lock
    find "$root/scripts/fixtures" -type f ! -path '*/__pycache__/*' -exec shasum -a 256 {} + | sort
    for binary in "${OUT:-$root/build/rust}/darwin-arm64/vmctl" "${OUT:-$root/build/rust}/darwin-arm64/clankerd-vmspawn" "${OUT:-$root/build/rust}/linux-arm64/clankerd-guestd" "${OUT:-$root/build/rust}/linux-arm64/dhcpcd"; do
      shasum -a 256 "$binary"
    done
  } | shasum -a 256 | awk '{print $1}'
}
failures=0
: > "$work/results.txt"
for stage in "${stages[@]}"; do
  fingerprint "$stage" > "$work/$stage.fingerprint"
  if [ -n "$resume" ] && [ -f "$resume/$stage.passed" ] && [ -f "$resume/$stage.fingerprint" ] && cmp -s "$work/$stage.fingerprint" "$resume/$stage.fingerprint"; then
    cp "$resume/$stage.log" "$work/$stage.log"
    cp "$resume/$stage.passed" "$work/$stage.passed"
    echo "REUSED $stage (unchanged inputs; evidence: $resume)" | tee -a "$work/results.txt"
    continue
  fi
  echo "RUN $stage"
  started=$SECONDS
  PF_TEST_STATE_POLICY=${PF_TEST_STATE_POLICY:-if-bound} "$root/scripts/$stage.sh" > "$work/$stage.log" 2>&1 &
  stage_pid=$!
  while kill -0 "$stage_pid" 2>/dev/null; do
    sleep 15
    if kill -0 "$stage_pid" 2>/dev/null; then echo "  $stage running ($((SECONDS-started))s); log: $work/$stage.log"; fi
  done
  if wait "$stage_pid"; then
    echo "PASS $stage ($((SECONDS-started))s)" | tee -a "$work/results.txt"
    date -u +%FT%TZ > "$work/$stage.passed"
    tail -3 "$work/$stage.log"
  else
    failures=$((failures+1))
    echo "FAIL $stage ($((SECONDS-started))s)" | tee -a "$work/results.txt"
    tail -45 "$work/$stage.log"
  fi
done
echo 'Suite results:'
cat "$work/results.txt"
echo "Full logs and host preflight: $work"
echo "Resume unchanged successful gates: scripts/m2-run-network-gates.sh --resume $work"
echo 'These gates do not complete routed production isolation, cross-network isolation with another VMM, sleep/wake or independent LAN-peer acceptance.'
[ "$failures" = 0 ]
