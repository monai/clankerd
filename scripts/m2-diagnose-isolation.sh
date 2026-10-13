#!/usr/bin/env bash
# Native-only reduction of shared/isolated vmnet interface creation failure.
set -Euo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
helper=${CLANKERD_VMNET_HELPER:-/opt/vmnet-helper/bin/vmnet-helper}
sw_vers
sudo --non-interactive -- "$helper" --version || exit 1
python3 "$root/scripts/fixtures/vmnet-isolation-diagnostic.py"
verdict=$?
echo 'Relevant native vmnet log entries (creation only):'
/usr/bin/log show --style compact --last 3m --predicate 'process == "vmnetd" OR subsystem BEGINSWITH "com.apple.vmnet"' 2>/dev/null | tail -120 || true
exit "$verdict"
