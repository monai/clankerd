#!/usr/bin/env bash
set -euo pipefail
[ "$(uname -s)" = Linux ] || { echo 'Requires Linux epoll'; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
src=${1:?Usage: test-dhcpcd-eloop.sh DHCPCD_SOURCE_DIRECTORY}
work=$(mktemp -d /tmp/clankerd-eloop-test.XXXXXX)
trap 'rm -rf "$work"' EXIT
"${CC:-cc}" -D_GNU_SOURCE -I "$src/src" \
  "$root/scripts/fixtures/dhcpcd-eloop-reuse.c" "$src/src/eloop.c" -o "$work/repro"
"$work/repro"
