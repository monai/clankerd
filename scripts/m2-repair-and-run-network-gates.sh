#!/usr/bin/env bash
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
binary=${OUT:-$root/build/rust}/darwin-arm64/clankerd-vmspawn
for attempt in $(seq 1 10); do
  formulas=$(python3 "$root/scripts/fixtures/macos-linked-dependencies.py" "$binary")
  [ -n "$formulas" ] || break
  while IFS= read -r formula; do
    echo "Repairing missing linked dependency formula: $formula"
    if brew list --versions "$formula" >/dev/null 2>&1; then brew reinstall "$formula"; else brew install "$formula"; fi
  done <<< "$formulas"
done
remaining=$(python3 "$root/scripts/fixtures/macos-linked-dependencies.py" "$binary")
[ -z "$remaining" ] || { echo "FAIL unresolved dependencies: $remaining" >&2; exit 1; }
export DYLD_FALLBACK_LIBRARY_PATH=${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}
python3 - "$binary" <<'PYLOAD'
import subprocess,sys
result=subprocess.run([sys.argv[1]],capture_output=True,text=True)
assert result.returncode==1 and '--spec is required' in result.stderr,(result.returncode,result.stderr)
print('PASS actual vmspawn loader reached argument validation without starting a VM')
PYLOAD
sudo -v
exec "$root/scripts/m2-run-network-gates.sh"
