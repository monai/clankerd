#!/usr/bin/env python3
"""Assemble one guest root from locked Alpine binary packages on any build host."""
import shutil
from pathlib import Path
import sys

out, rootfs = map(Path, sys.argv[1:3])
separator = sys.argv.index('--required')
roots = [Path(arg).resolve() for arg in sys.argv[3:separator]]
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
# Management owns its account database and configuration; package defaults
# never select identities or overwrite the checked-in filesystem policy.
overlay(rootfs, out)
for name in ['var/lib/dhcpcd', 'run/dhcpcd']:
    (out / name).mkdir(parents=True, exist_ok=True)
(out / 'etc/shadow').chmod(0o600)
# busybox's distribution trigger populates applet links; the minirootfs has
# them already. Require the shell and nologin used by dhcpcd's packaged hook.
for required in required_files:
    if not (out / required).exists():
        raise RuntimeError('missing guest runtime input: ' + required)
    data=(out / required).read_bytes()
    if (data.startswith(b'\x7fELF') or required.startswith(('bin/', 'sbin/', 'usr/bin/', 'usr/sbin/'))) and not (out / required).stat().st_mode & 0o111:
        raise RuntimeError('guest runtime program is not executable: ' + required)
    if data.startswith(b'\x7fELF') and (data[4]!=2 or int.from_bytes(data[18:20], 'little')!=183):
        raise RuntimeError('guest binary is not AArch64: ' + required)
# Runtime ownership is root in libkrun's UID mapping; host build owns no devices.
for path in out.rglob('.complete'):
    path.unlink()
