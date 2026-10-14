#!/usr/bin/env bash
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
dependency=/opt/homebrew/opt/libepoxy/lib/libepoxy.0.dylib
if [ ! -f "$dependency" ]; then
  echo "Repairing missing libkrun dependency: $dependency"
  if brew list --versions libepoxy >/dev/null 2>&1; then
    brew reinstall libepoxy
  else
    brew install libepoxy
  fi
fi
[ -f "$dependency" ] || { echo "FAIL dependency still missing: $dependency" >&2; exit 1; }
echo 'Verifying installed libkrun linkage:'
brew linkage --test libkrun
sudo -v
exec "$root/scripts/m2-run-network-gates.sh"
