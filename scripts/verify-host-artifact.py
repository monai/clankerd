#!/usr/bin/env python3
"""Check every Mach-O import/rpath in the relocated artifact without loading it."""
from pathlib import Path
import struct
import sys

host = Path(sys.argv[1]) / 'darwin-arm64'
for path in [*(host / 'lib').glob('*.dylib'), *(host / name for name in ['vmctl', 'clankerd-vmspawn', 'clankerd-network'])]:
    if path.is_symlink():
        continue
    data = path.read_bytes()
    assert struct.unpack_from('<II', data) == (0xfeedfacf, 0x100000c), path
    offset = 32
    imports = []
    rpaths = []
    for _ in range(struct.unpack_from('<I', data, 16)[0]):
        command, size = struct.unpack_from('<II', data, offset)
        if command in {0xc, 0x80000018, 0x8000001f, 0x20, 0x80000023, 0x8000001c}:
            start = offset + struct.unpack_from('<I', data, offset + 8)[0]
            value = data[start:offset+size].split(b'\0')[0].decode()
            if command == 0x8000001c:
                rpaths.append(value)
                assert value in {'@loader_path', '@executable_path/lib'}, (path, value)
            else:
                imports.append(value)
                if path.name == 'clankerd-network':
                    assert value.startswith(('/usr/lib/', '/System/Library/')), (path, value)
                if value.startswith(('/usr/lib/', '/System/Library/')):
                    pass
                elif value.startswith('@loader_path/'):
                    assert (path.parent / value[len('@loader_path/'):]).is_file(), (path, value)
                elif value.startswith('@rpath/'):
                    assert (host / 'lib' / value[len('@rpath/'):]).is_file(), (path, value)
                else:
                    raise AssertionError((path, value))
        offset += size
    if path.name == 'clankerd-vmspawn':
        assert '@executable_path/lib' in rpaths, (path, rpaths)
        assert '@rpath/libkrun.1.dylib' in imports, (path, imports)
print('PASS arm64 host import closure and relative library search paths')
