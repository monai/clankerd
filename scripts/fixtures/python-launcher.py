#!/usr/bin/env python3
"""Create a fixture entry point using the current Python interpreter directly.

On macOS, routing its shebang through /usr/bin/env strips DYLD_* settings.
"""
from pathlib import Path
import sys

source, launcher = map(Path, sys.argv[1:])
launcher.write_text(
    f'#!{sys.executable}\n'
    'import runpy\n'
    f'runpy.run_path({str(source.resolve())!r}, run_name="__main__")\n')
launcher.chmod(0o755)
