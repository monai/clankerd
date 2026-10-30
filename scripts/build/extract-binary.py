#!/usr/bin/env python3
"""Extract one verified distribution archive without root or install scripts."""
from pathlib import Path
import sys
import tarfile
import posixpath

archive, destination = map(Path, sys.argv[1:])
destination.mkdir(parents=True)
with tarfile.open(archive) as source:
    for member in source:
        if member.name.startswith('/') or '..' in member.name.split('/'):
            raise ValueError('archive member must remain inside the guest root: ' + member.name)
        # APK metadata and scripts are evidence, never host-executed code.
        # Alpine tar paths begin './'; APK control members begin '.'.
        if member.name.startswith('.') and not member.name.startswith('./') and member.name != '.':
            member.name = 'package-metadata/' + member.name
        if member.issym() and member.linkname.startswith('/'):
            member.linkname = posixpath.relpath(member.linkname.lstrip('/'), posixpath.dirname(member.name))
        member.uid = member.gid = 0
        source.extract(member, destination, filter='data')
