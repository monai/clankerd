#!/usr/bin/env python3
"""Compare loader environment inheritance across the fixture's interpreter boundary."""
import os
from pathlib import Path
import subprocess
import sys


def main():
    if sys.platform != 'darwin':
        print('Requires macOS', file=sys.stderr)
        return 1
    print('[1/3] Locate the installed firmware library', flush=True)
    prefix = subprocess.check_output(['brew', '--prefix', 'libkrunfw'], text=True).strip()
    library = Path(prefix) / 'lib'
    print(library, flush=True)
    environment = dict(os.environ, DYLD_LIBRARY_PATH=str(library))
    probe = 'import os; print("DYLD_LIBRARY_PATH=" + repr(os.getenv("DYLD_LIBRARY_PATH")))'
    for step, (name, command) in enumerate([
        ('Direct Python', [sys.executable, '-c', probe]),
        ('Fixture interpreter boundary', ['/usr/bin/env', sys.executable, '-c', probe]),
    ], 2):
        print(f'[{step}/3] {name}', flush=True)
        subprocess.run(command, env=environment, check=True, timeout=10)
    if sys.argv[1:] == ['--lifetimes']:
        krun = subprocess.check_output(['brew', '--prefix', 'libkrun'], text=True).strip()
        flags = environment.get('RUSTFLAGS', '')
        environment['RUSTFLAGS'] = f'{flags} -L native={krun}/lib'.strip()
        root = Path(__file__).resolve().parents[1]
        print('Running lifetimes acceptance with the inspected library locations', flush=True)
        return subprocess.call(['bash', str(root / 'scripts/accept-native-build.sh'),
                                'm2-smoke-lifetimes'], env=environment, cwd=root)
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, subprocess.SubprocessError) as error:
        sys.exit(f'FAIL: {error}')
