#!/usr/bin/env python3
"""Check owned executable architecture before atomic publication.

The native linker and dyld resolve installed libraries.
"""
from pathlib import Path
import struct
import sys

host = Path(sys.argv[1]) / 'darwin-arm64'
for name in ['vmctl', 'clankerd-vmspawn', 'clankerd-network']:
    path = host / name
    with path.open('rb') as executable:
        assert executable.read(8) == struct.pack('<II', 0xfeedfacf, 0x100000c), path
print('PASS arm64 host executables')
