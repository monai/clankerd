#!/usr/bin/env bash
# Verifies the artifacts produced by `make rust-build` on Linux or macOS.
#   - darwin binaries are arm64 Mach-O
#   - clankerd-vmspawn links Homebrew's libkrun and carries the hypervisor entitlement in its code signature
#   - clankerd-guestd is a static aarch64 musl ELF
#   - the static e2fsprogs binaries sit next to it (guest boot directory)
set -euo pipefail

out=${1:-build/rust}
if command -v readelf >/dev/null 2>&1; then
  elf_reader=readelf
elif command -v greadelf >/dev/null 2>&1; then
  elf_reader=greadelf
else
  echo "readelf missing: on macOS, install Homebrew binutils and add its bin directory to PATH" >&2
  exit 1
fi
fail=0
check() { # description, command...
  local d=$1; shift
  if "$@" >/dev/null 2>&1; then echo "ok   $d"; else echo "FAIL $d"; fail=1; fi
}

macho_arm64() { # arm64 Mach-O magic cffaedfe, cputype 0x0100000c
  [ "$(od -An -tx4 -N8 "$1" | tr -d ' ')" = "feedfacf0100000c" ]
}
has_entitlement() {
  rcodesign print-signature-info "$1" | grep -q 'com.apple.security.hypervisor'
}
static_aarch64_elf() { # ELF, machine 0xb7, no PT_INTERP / PT_DYNAMIC
  [ "$(od -An -c -N4 "$1" | tr -d ' ')" = '177ELF' ] || return 1
  [ "$(od -An -tx2 -j18 -N2 "$1" | tr -d ' ')" = "00b7" ] || return 1
  local headers
  headers=$("$elf_reader" -lW "$1") || return 1
  ! printf '%s\n' "$headers" | grep -qE 'INTERP|DYNAMIC'
}

for b in vmctl clankerd-vmspawn; do
  check "darwin-arm64/$b is arm64 Mach-O" macho_arm64 "$out/darwin-arm64/$b"
done
check "darwin-arm64/clankerd-vmspawn has hypervisor entitlement" \
  has_entitlement "$out/darwin-arm64/clankerd-vmspawn"
check "darwin-arm64/clankerd-vmspawn links /opt/homebrew/lib/libkrun.1.dylib" \
  grep -aq '/opt/homebrew/lib/libkrun.1.dylib' "$out/darwin-arm64/clankerd-vmspawn"
check "linux-arm64/clankerd-guestd is static aarch64 ELF" \
  static_aarch64_elf "$out/linux-arm64/clankerd-guestd"

# The guest boot directory ships static e2fsprogs next to guestd.
for b in mke2fs e2fsck resize2fs debugfs; do
  check "linux-arm64/$b is static aarch64 ELF" static_aarch64_elf "$out/linux-arm64/$b"
done

exit $fail
