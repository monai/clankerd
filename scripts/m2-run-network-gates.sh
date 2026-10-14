#!/usr/bin/env bash
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
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
stages=(m2-test-isolation-pf-dual m2-smoke-isolation-multicast m2-smoke-vmnet m2-smoke-ipv6 m2-smoke-lifetimes m2-smoke-recovery m2-smoke-bridged m2-smoke-test)
failures=0
for stage in "${stages[@]}"; do
  echo "RUN $stage"
  started=$SECONDS
  "$root/scripts/$stage.sh" > "$work/$stage.log" 2>&1 &
  stage_pid=$!
  while kill -0 "$stage_pid" 2>/dev/null; do
    sleep 15
    if kill -0 "$stage_pid" 2>/dev/null; then echo "  $stage running ($((SECONDS-started))s); log: $work/$stage.log"; fi
  done
  if wait "$stage_pid"; then
    echo "PASS $stage ($((SECONDS-started))s)" | tee -a "$work/results.txt"
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
echo 'These existing gates do not complete production isolation, another-VMM interoperability, sleep/wake or independent LAN-peer acceptance.'
[ "$failures" = 0 ]
