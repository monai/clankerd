#!/usr/bin/env python3
"""Content inventory for one completed build output (including modes and links)."""
import hashlib
import json
from pathlib import Path
import sys


def inventory(root):
    rows = {'.': ['directory', root.stat().st_mode & 0o7777]}
    for path in sorted(root.rglob('*')):
        relative = path.relative_to(root).as_posix()
        if relative == '.complete':
            continue
        stat = path.lstat()
        if path.is_symlink():
            rows[relative] = ['link', str(path.readlink())]
        elif path.is_file():
            with path.open('rb') as stream:
                rows[relative] = ['file', stat.st_mode & 0o7777, hashlib.file_digest(stream, 'sha256').hexdigest()]
        elif path.is_dir():
            rows[relative] = ['directory', stat.st_mode & 0o7777]
        else:
            raise ValueError(f'unexpected special file: {path}')
    return rows


if __name__ == '__main__':
    root = Path(sys.argv[2])
    if sys.argv[1] == 'record':
        (root / '.complete').write_text(json.dumps({'files': inventory(root)}, sort_keys=True) + '\n')
    elif sys.argv[1] == 'check':
        try:
            valid = json.loads((root / '.complete').read_text()).get('files') == inventory(root)
        except (OSError, ValueError):
            valid = False
        sys.exit(0 if valid else 1)
