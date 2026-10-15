#!/usr/bin/env bash
# Disposable capability build; never installs or replaces the system helper.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac SDK' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:?Usage: build-prefix-test-helper.sh OWNED_OUTPUT_DIR}
mkdir -p "$out"
out=$(cd "$out" && pwd)
commit=222c121ba31e49856c9ec6c3a14426b49d77c8fe
sha=1d0e81592837742f8eb74f9b91b2d6b27dd558c71cbc927d42863b71e764404a
curl -fsSL "https://codeload.github.com/nirs/vmnet-helper/tar.gz/$commit" -o "$out/source.tar.gz"
printf '%s  %s\n' "$sha" "$out/source.tar.gz" | shasum -a 256 -c -
mkdir "$out/source"
tar -C "$out/source" --strip-components=1 -xf "$out/source.tar.gz"
patch -s -d "$out/source" -p1 < "$root/scripts/patches/vmnet-helper-nat66-prefix.patch"
printf '#define GIT_VERSION "v0.13.0-prefix-probe"\n#define GIT_COMMIT "%s"\n' "$commit" > "$out/version.h"
xcrun clang -std=gnu99 -O2 -fblocks -mmacosx-version-min=15.0 \
  -I "$out" -I "$out/source/programs" -I "$out/source/broker" \
  "$out/source/programs/helper.c" "$out/source/programs/options.c" \
  "$out/source/programs/os.c" "$out/source/broker/client.c" \
  -framework vmnet -framework CoreFoundation -o "$out/vmnet-helper"
"$out/vmnet-helper" --version
