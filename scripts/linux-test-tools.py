#!/usr/bin/env python3
"""Expose the packaged AArch64 tools to Linux tests without installing a loader."""
from pathlib import Path
import shlex
import sys

root, output = map(lambda p: Path(p).resolve(), sys.argv[1:])
output.mkdir(parents=True)
loader = root / 'lib/ld-musl-aarch64.so.1'
for name, relative in [('mke2fs', 'sbin/mke2fs'), ('e2fsck', 'sbin/e2fsck'),
                       ('resize2fs', 'usr/sbin/resize2fs'), ('debugfs', 'usr/sbin/debugfs')]:
    wrapper = output / name
    wrapper.write_text('#!/bin/sh\nexport MKE2FS_CONFIG=' + shlex.quote(str(root / 'etc/mke2fs.conf')) + '\nexec ' +
                       shlex.join([str(loader), '--library-path', str(root / 'lib') + ':' + str(root / 'usr/lib'), str(root / relative)]) + ' "$@"\n')
    wrapper.chmod(0o755)
