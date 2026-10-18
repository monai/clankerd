#!/usr/bin/env python3
"""Assemble one guest root from locked Alpine binary packages on any build host."""
import shutil
from pathlib import Path
import sys

out = Path(sys.argv[1])
separator = sys.argv.index('--required')
roots = [Path(arg).resolve() for arg in sys.argv[2:separator]]
required_files = sys.argv[separator+1:]
def overlay(source, target):
    target.mkdir(parents=True, exist_ok=True)
    for path in source.iterdir():
        if path.name in {'.complete', 'package-metadata'}:
            continue
        destination = target / path.name
        if path.is_symlink():
            destination.unlink(missing_ok=True)
            destination.symlink_to(path.readlink())
        elif path.is_dir():
            overlay(path, destination)
        else:
            if destination.is_symlink():
                destination.unlink()
            shutil.copy2(path, destination)
    target.chmod(source.stat().st_mode & 0o7777)

for root in roots:
    overlay(root, out)
# APK dhcpcd.pre-install creates a system account/chroot. Reproduce that
# provisioning without executing target binaries on the build host.
passwd = out / 'etc/passwd'
group = out / 'etc/group'
if not any(line.startswith('dhcpcd:') for line in passwd.read_text().splitlines()):
    ids = {int(line.split(':')[2]) for line in passwd.read_text().splitlines() if line}
    ids.update(int(line.split(':')[2]) for line in group.read_text().splitlines() if line)
    uid = next(i for i in range(100, 1000) if i not in ids)
    with passwd.open('a') as stream:
        stream.write(f'dhcpcd:x:{uid}:{uid}:dhcpcd:/var/lib/dhcpcd:/sbin/nologin\n')
    with group.open('a') as stream:
        stream.write(f'dhcpcd:x:{uid}:\n')
for name in ['var/lib/dhcpcd', 'run/dhcpcd', 'usr/lib/clankerd']:
    (out / name).mkdir(parents=True, exist_ok=True)
hook = out / 'usr/lib/clankerd/network-hook'
hook.write_text('#!/bin/sh\nexec /clankerd-guestd network-hook\n')
hook.chmod(0o755)
for name, target in [('mke2fs','sbin/mke2fs'), ('e2fsck','sbin/e2fsck'), ('resize2fs','usr/sbin/resize2fs'), ('debugfs','usr/sbin/debugfs')]:
    (out / name).symlink_to(target)
# busybox's distribution trigger populates applet links; the minirootfs has
# them already. Require the shell and nologin used by dhcpcd's packaged hook.
for required in required_files:
    if not (out / required).exists():
        raise RuntimeError('missing guest runtime input: ' + required)
    data=(out / required).read_bytes()
    if data.startswith(b'\x7fELF') and (data[4]!=2 or int.from_bytes(data[18:20], 'little')!=183):
        raise RuntimeError('guest binary is not AArch64: ' + required)
# Runtime ownership is root in libkrun's UID mapping; host build owns no devices.
for path in out.rglob('.complete'):
    path.unlink()
