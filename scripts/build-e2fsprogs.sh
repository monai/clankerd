#!/usr/bin/env bash
# Builds static aarch64 (musl) e2fsprogs binaries for the guest boot directory
# with the zig pinned in mise.toml, from the upstream release tarball.
#
#   scripts/build-e2fsprogs.sh [OUT_DIR]     (default: build/rust/linux-arm64)
#
# Installs mke2fs, e2fsck, resize2fs and debugfs into OUT_DIR, next to
# clankerd-guestd (guestd looks for mke2fs in its own directory).
# The tarball is checksum-verified against the pin below, which equals the
# entry in the upstream-signed sha256sums.asc of the same release directory.
set -euo pipefail

VERSION=1.47.2
SHA256=08242e64ca0e8194d9c1caad49762b19209a06318199b63ce74ae4ef2d74e63c
URL=https://mirrors.edge.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v$VERSION/e2fsprogs-$VERSION.tar.xz
TARGET=aarch64-linux-musl

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/build/rust/linux-arm64}
cache=${E2FSPROGS_CACHE:-$root/build/e2fsprogs}
mkdir -p "$out" "$cache"
out=$(cd "$out" && pwd)

tarball=$cache/e2fsprogs-$VERSION.tar.xz
if [ ! -f "$tarball" ]; then
  curl -sSfL -o "$tarball.part" "$URL"
  mv "$tarball.part" "$tarball"
fi
echo "$SHA256  $tarball" | sha256sum -c - >/dev/null || {
  echo "e2fsprogs tarball checksum mismatch" >&2
  rm -f "$tarball"
  exit 1
}

src=$cache/src-$VERSION-$TARGET
rm -rf "$src"
mkdir -p "$src"
tar -C "$src" --strip-components=1 -xf "$tarball"

# zig cc wrappers: configure wants a single executable for CC.
wrap=$cache/wrap-$TARGET
mkdir -p "$wrap"
printf '#!/bin/sh\nexec zig cc -target %s "$@"\n' "$TARGET" >"$wrap/cc"
printf '#!/bin/sh\nexec zig ar "$@"\n' >"$wrap/ar"
printf '#!/bin/sh\nexec zig ranlib "$@"\n' >"$wrap/ranlib"
chmod +x "$wrap"/*

cd "$src"
export CC="$wrap/cc" AR="$wrap/ar" RANLIB="$wrap/ranlib" BUILD_CC=cc
./configure --host=aarch64-linux-musl --disable-nls --disable-fuse2fs \
  --disable-uuidd --disable-defrag \
  --disable-e2initrd-helper \
  LDFLAGS="-static -s" >/dev/null
make -j"$(nproc)" -s >/dev/null

for b in misc/mke2fs e2fsck/e2fsck resize/resize2fs debugfs/debugfs; do
  install -m 0755 "$b" "$out/$(basename "$b")"
done
echo "installed $(ls "$out" | tr '\n' ' ') into $out"
