#!/usr/bin/env bash
# Pinned dual-stack DHCP/RA engine, independent of the workload image.
set -euo pipefail
VERSION=10.3.0
# SHA256 matches the digest published on the upstream GitHub release asset.
SHA256=06e4c1aaf958523f3fd1c57258c613c6c7ae56b8f1d678fa7943495d5ea6aeb5
URL=https://github.com/NetworkConfiguration/dhcpcd/releases/download/v$VERSION/dhcpcd-$VERSION.tar.xz
TARGET=aarch64-linux-musl
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/build/rust/linux-arm64}
cache=${DHCPCD_CACHE:-$root/build/dhcpcd}
mkdir -p "$out" "$cache"
out=$(cd "$out" && pwd)
cache=$(cd "$cache" && pwd)
tarball=$cache/dhcpcd-$VERSION.tar.xz
if [ ! -f "$tarball" ]; then
  curl -sSfL -o "$tarball.part" "$URL"
  mv "$tarball.part" "$tarball"
fi
if command -v sha256sum >/dev/null 2>&1; then
  checksum=(sha256sum)
else
  checksum=(shasum -a 256)
fi
echo "$SHA256  $tarball" | "${checksum[@]}" -c - >/dev/null || {
  echo "dhcpcd tarball checksum mismatch" >&2
  rm -f "$tarball"
  exit 1
}
src=$cache/src-$VERSION-$TARGET
rm -rf "$src"
mkdir -p "$src"
tar -C "$src" --strip-components=1 -xf "$tarball"
wrap=$cache/wrap-$TARGET
mkdir -p "$wrap"
printf '#!/bin/sh\nexec zig cc -target %s "$@"\n' "$TARGET" >"$wrap/cc"
chmod +x "$wrap/cc"
cd "$src"
# Guestd supervises this root process and supplies its binary hook; no image
# account, shell hook, udev service or privilege-separation chroot is required.
# Upstream derives OS from --host, not --target; otherwise a Mac build
# omits the Linux source files, netlink includes and libc feature macros.
CC="$wrap/cc" LDFLAGS="-static -s" ./configure --host="$TARGET" \
  --disable-privsep --without-dev --dbdir=/run/clankerd/dhcpcd \
  --rundir=/run/clankerd/dhcpcd >/dev/null
if command -v nproc >/dev/null 2>&1; then
  jobs=$(nproc)
else
  jobs=$(sysctl -n hw.ncpu)
fi
make -j"$jobs" -s >/dev/null
install -m 0755 src/dhcpcd "$out/dhcpcd"
install -m 0644 LICENSE "$out/dhcpcd.LICENSE"
echo "installed static dhcpcd $VERSION into $out"
