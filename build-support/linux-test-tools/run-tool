#!/bin/sh
# This directory contains a management symlink and tool-name symlinks to us.
set -eu
launcher=$(readlink -f -- "$0")
root=$(CDPATH= cd -- "$(dirname -- "$launcher")/management" && pwd -P)
name=${0##*/}
case "$name" in
    mke2fs|e2fsck) program=$root/sbin/$name ;;
    resize2fs|debugfs) program=$root/usr/sbin/$name ;;
    *) echo "Unknown packaged test tool: $name" >&2; exit 2 ;;
esac
export MKE2FS_CONFIG=$root/etc/mke2fs.conf
exec "$root/lib/ld-musl-aarch64.so.1" \
    --library-path "$root/lib:$root/usr/lib" "$program" "$@"
