#!/usr/bin/env python3
"""Configure installed Homebrew libraries, then delegate to a core Make target."""
import os
from pathlib import Path
import subprocess
import sys


def main():
    make, *targets = sys.argv[1:]
    if subprocess.check_output(['uname', '-sm'], text=True).strip() != 'Darwin arm64':
        sys.exit('Homebrew entrypoints require native Apple Silicon macOS')
    dependencies = subprocess.check_output(
        ['brew', 'deps', '--installed', '--formula', 'libkrun'], text=True).splitlines()
    formulas = dict.fromkeys(['libkrun', 'libkrunfw', 'xz', *dependencies])
    environment = {key: value for key, value in os.environ.items() if not key.startswith('DYLD_')}
    encoded = environment.get('HOST_RUSTFLAGS', environment.get('CARGO_ENCODED_RUSTFLAGS'))
    if encoded is not None:
        flags = encoded.split('\x1f') if encoded else []
    else:
        flags = environment.get('RUSTFLAGS', environment.get(
            'CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS', '')).split()
    for formula in formulas:
        prefix = subprocess.check_output(['brew', '--prefix', formula], text=True).strip()
        library = str(Path(prefix) / 'lib')
        flags.extend(['-Lnative='+library,
                      '-Clink-arg=-Xlinker', '-Clink-arg=-rpath',
                      '-Clink-arg=-Xlinker', '-Clink-arg='+library])
    environment['HOST_RUSTFLAGS'] = '\x1f'.join(flags)
    os.execvpe(make, [make, *targets], environment)


if __name__ == '__main__':
    try:
        main()
    except (OSError, subprocess.SubprocessError) as error:
        sys.exit(f'Homebrew configuration failed: {error}')
