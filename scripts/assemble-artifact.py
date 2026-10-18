#!/usr/bin/env python3
"""Assemble and sign completed graph outputs; never compile or download inputs."""
from pathlib import Path
import shutil
import subprocess
import sys

out, work, lock = map(Path, sys.argv[1:4])
host = out / 'darwin-arm64'
guest = out / 'linux-arm64'
shutil.copytree((work / 'host-rust').resolve(), host, ignore=shutil.ignore_patterns('.complete'))
shutil.copytree((work / 'guest-rust').resolve(), guest, ignore=shutil.ignore_patterns('.complete'))
shutil.copytree((work / 'management').resolve(), guest / 'management', symlinks=True, ignore=shutil.ignore_patterns('.complete'))
shutil.copytree((work / 'host-libraries').resolve() / 'lib', host / 'lib', symlinks=True)
shutil.copytree((work / 'host-libraries').resolve() / 'licenses', out / 'licenses/host')
shutil.copy2(lock, out / 'inputs.mk')
for name in sys.argv[4:]:
    name, path = name.split('=', 1)
    target = out / 'licenses/texts' / (name + '.txt')
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, target)
shutil.copy2(guest / 'clankerd-guestd', guest / 'management/clankerd-guestd')
# Preserve package metadata and scripts as provisioning/provenance evidence.
for package in (work / 'packages').iterdir():
    metadata = package / 'package-metadata'
    if metadata.exists():
        shutil.copytree(metadata, out / 'licenses/guest' / package.name)
shutil.copy2(guest / 'management/lib/apk/db/installed', out / 'licenses/alpine-base-installed')
for path in (host / 'lib').glob('*.dylib'):
    if not path.is_symlink():
        subprocess.run(['rcodesign', 'sign', str(path)], check=True)
for name in ['vmctl', 'clankerd-vmspawn', 'clankerd-network']:
    command = ['rcodesign', 'sign', '--binary-identifier', 'io.clankerd.' + name]
    if name == 'clankerd-vmspawn':
        command += ['-e', 'crates/clankerd-vmspawn/entitlements.plist']
    subprocess.run(command + [str(host / name)], check=True)

subprocess.run(['python3', 'scripts/verify-host-artifact.py', str(out)], check=True)
