#!/usr/bin/env python3
"""Place and relocate arm64 Mach-O dylibs from locked bottles, before signing."""
import shutil
import struct
import sys
from pathlib import Path

out = Path(sys.argv[1])
lib = out / 'lib'
lib.mkdir(parents=True)
licenses = out / 'licenses'
licenses.mkdir()
# Load-command strings only shrink; no binary data offsets need to move.
# An unknown non-system import is an error, never a host fallback.
commands = {0xc, 0xd, 0x80000018, 0x8000001f, 0x20, 0x80000023}
separator = sys.argv.index('--required')
for source in [Path(value).resolve() for value in sys.argv[2:separator]]:
    for path in source.rglob('*'):
        if path.is_symlink() or not path.is_file():
            continue
        if path.suffix == '.dylib':
            target = lib / path.name
            if target.exists():
                raise RuntimeError('duplicate host library: ' + path.name)
            shutil.copy2(path, target)
        if path.name.startswith(('LICENSE', 'COPYING')):
            target = licenses / source.name / path.relative_to(source)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
# Preserve ABI soname links that Cargo and libkrun's firmware loader use.
(lib / 'libkrun.1.dylib').symlink_to('libkrun.1.19.6.dylib')
for path in lib.iterdir():
    if path.is_symlink():
        continue
    data = bytearray(path.read_bytes())
    if struct.unpack_from('<II', data) != (0xfeedfacf, 0x100000c):
        raise RuntimeError('expected arm64 Mach-O: ' + str(path))
    count = struct.unpack_from('<I', data, 16)[0]
    offset = 32
    for _ in range(count):
        command, size = struct.unpack_from('<II', data, offset)
        if size < 8 or offset + size > len(data):
            raise RuntimeError('invalid Mach-O command')
        if command in commands or command == 0x8000001c:
            start = offset + struct.unpack_from('<I', data, offset + 8)[0]
            end = data.index(0, start, offset + size)
            old = bytes(data[start:end]).decode()
            replacement = old
            if command == 0xd:
                replacement = path.name if path.name == 'libkrunfw.5.dylib' else '@rpath/' + path.name
            elif command == 0x8000001c:
                replacement = '@loader_path'
            elif not old.startswith(('/usr/lib/', '/System/Library/')):
                dependency = old.rsplit('/', 1)[-1]
                if not (lib / dependency).exists():
                    raise RuntimeError(f'unbundled dependency {old} in {path.name}')
                replacement = '@loader_path/' + dependency
            encoded = replacement.encode()
            if len(encoded) > end - start:
                raise RuntimeError('insufficient Mach-O string space: ' + old)
            data[start:end] = encoded + bytes(end - start - len(encoded))
        offset += size
    path.write_bytes(data)

for required in sys.argv[separator+1:]:
    if not (out / required).is_file():
        raise RuntimeError('missing required host runtime: ' + required)

# Cargo's lzma-sys probes this package instead of the host's Homebrew install.
if (lib / 'liblzma.5.dylib').exists():
    (lib / 'liblzma.dylib').symlink_to('liblzma.5.dylib')
    metadata = out / 'pkgconfig'
    metadata.mkdir()
    (metadata / 'liblzma.pc').write_text(
        'libdir=${pcfiledir}/../lib\nName: liblzma\nDescription: pinned host liblzma\n'
        'Version: 5.8.4\nLibs: -L${libdir} -llzma\n')
