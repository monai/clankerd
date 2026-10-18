#!/usr/bin/env python3
"""Record a graph input set, including additions, removals and content changes."""
import hashlib
import json
import os
from pathlib import Path
import sys

output = Path(sys.argv[1])
rows = {}
for name in sys.argv[2:]:
    with open(name, 'rb') as stream:
        rows[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
content = json.dumps(rows, sort_keys=True) + '\n'
output.parent.mkdir(parents=True, exist_ok=True)
if not output.exists() or output.read_text() != content:
    temporary = output.with_suffix('.tmp-' + str(os.getpid()))
    temporary.write_text(content)
    os.replace(temporary, output)
