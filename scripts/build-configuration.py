#!/usr/bin/env python3
"""Invalidate graph outputs when the selected compilation environment changes."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

path = Path(sys.argv[1])
variables = ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'MACOSX_DEPLOYMENT_TARGET',
             'DEVELOPER_DIR', 'CC', 'CFLAGS', 'LDFLAGS')
configuration = {name: os.environ.get(name) for name in variables}
configuration.update({key: value for key, value in os.environ.items() if key.startswith(('CARGO_PROFILE_', 'CARGO_TARGET_'))})
configuration['arguments'] = sys.argv[2:]
for tool in ['rustc', 'cargo', 'zig', 'rcodesign']:
    configuration[tool] = [shutil.which(tool), subprocess.check_output([tool, '--version' if tool != 'zig' else 'version'], text=True).strip()]
content = json.dumps(configuration, sort_keys=True) + '\n'
path.parent.mkdir(parents=True, exist_ok=True)
if not path.exists() or path.read_text() != content:
    temporary = path.with_suffix('.tmp-' + str(os.getpid()))
    temporary.write_text(content)
    os.replace(temporary, path)
