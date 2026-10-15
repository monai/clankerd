#!/usr/bin/env bash
set -Eeuo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
failures=0
for policy in floating if-bound; do
  echo "RUN held-state source-validation regression: $policy"
  if PF_TEST_STATE_POLICY=$policy "$root/scripts/m2-test-isolation-pf-state.sh"; then
    echo "PASS held-state source-validation regression: $policy"
  else
    failures=$((failures+1))
    echo "FAIL held-state source-validation regression: $policy"
  fi
done
[ "$failures" = 0 ]
