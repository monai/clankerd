#!/usr/bin/env python3
"""Atomically point the public artifact name at a completed immutable generation."""
import fcntl
import os
from pathlib import Path
import sys

source, output = map(Path, sys.argv[1:])
source = source.resolve()
output = output.absolute()
output.parent.mkdir(parents=True, exist_ok=True)
with (output.parent / ('.' + output.name + '.lock')).open('a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    if output.is_symlink() and output.resolve() == source:
        sys.exit(0)
    if output.exists() and not output.is_symlink():
        raise RuntimeError('artifact publication requires an absent path or symlink: ' + str(output))
    link = output.parent / ('.' + output.name + '-' + str(os.getpid()))
    try:
        link.symlink_to(os.path.relpath(source, output.parent))
        os.replace(link, output)
    finally:
        link.unlink(missing_ok=True)
