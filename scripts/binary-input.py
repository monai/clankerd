#!/usr/bin/env python3
"""Verify one locked binary input; atomically download it if absent."""
import fcntl
import hashlib
import os
from pathlib import Path
import sys
import tempfile
import urllib.request


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def fetch(path, expected, url):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.with_suffix('.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if path.exists():
            if digest(path) != expected:
                raise RuntimeError(f'checksum mismatch: {path}; remove this corrupt input to download again')
            return
        fd, name = tempfile.mkstemp(prefix='.download-', dir=path.parent)
        try:
            request = urllib.request.Request(url, headers={'Authorization': 'Bearer QQ=='}) if url.startswith('https://ghcr.io/') else url
            with os.fdopen(fd, 'wb') as out, urllib.request.urlopen(request) as response:
                import shutil
                shutil.copyfileobj(response, out)
            if digest(Path(name)) != expected:
                raise RuntimeError(f'checksum mismatch: {url}')
            os.replace(name, path)
        finally:
            Path(name).unlink(missing_ok=True)


if __name__ == '__main__':
    fetch(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
