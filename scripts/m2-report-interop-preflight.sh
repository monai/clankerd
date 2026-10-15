#!/usr/bin/env bash
set -Eeuo pipefail
work=${1:?Usage: m2-report-interop-preflight.sh retained-host-only-directory}
[ -d "$work" ] || exit 1
for file in qemu-version.txt qemu-netdevs.txt lima-instances.json lima-list.log; do
  if [ -f "$work/$file" ]; then echo "$file:"; cat "$work/$file"; fi
done
if [ ! -f "$work/lima-instances.json" ]; then
  echo 'Existing Lima instances (read only):'
  limactl list --json
fi
echo 'QEMU system firmware files:'
for directory in /opt/homebrew/share/qemu /opt/homebrew/share/qemu/firmware; do
  if [ -d "$directory" ]; then
    find "$directory" -maxdepth 1 -type f \( -name '*aarch64*' -o -name '*arm*' \) -print
  fi
done
