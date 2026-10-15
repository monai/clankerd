#!/usr/bin/env bash
# Own disposable QEMU guest; never modifies Lima/UTM configuration.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
export OUT=${OUT:-$root/build/rust}
export CLANKERD_VMSPAWN=$OUT/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$OUT/linux-arm64/clankerd-guestd
export DYLD_FALLBACK_LIBRARY_PATH=${DYLD_FALLBACK_LIBRARY_PATH:-/opt/homebrew/lib:/usr/local/lib:/usr/lib}
exec python3 "$root/scripts/fixtures/vmnet-other-vmm.py" "$root"
