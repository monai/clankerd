#!/usr/bin/env bash
set -Eeuo pipefail
python3 - "${1:?Usage: m2-report-other-vmm.sh retained-directory}" <<'PY'
from pathlib import Path
import re,sys
root=Path(sys.argv[1])
assert root.is_dir(),root
for name in ('qemu-version.txt','qemu-netdevs.txt','firmware.json','helper.log','qemu-console.log'):
    path=root/name
    if path.is_file():
        print('\n'+name+':')
        text=path.read_text(errors='replace')
        text=re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]','',text)
        lines=text.replace('\r','\n').splitlines()
        print('\n'.join(lines[-100:] if name=='qemu-console.log' else lines[-20:]))
print('\nSeed files:')
for path in sorted(root.rglob('*')):
    if path.is_file() and path.name in ('user-data','meta-data','network-config'):
        print(str(path.relative_to(root))+':')
        text=path.read_text(errors='replace')
        text=re.sub(r'ssh-ed25519\s+\S+', 'ssh-ed25519 [public key omitted]',text)
        print(text[:10000])
path=root/'commands.log'
if path.is_file():
    lines=path.read_text(errors='replace').splitlines()
    setup=[line for line in lines if any(word in line for word in ('qemu-img','hdiutil','ssh-keygen'))]
    print('\nSetup commands:\n'+'\n'.join(setup))
    print('\nLast management attempt:\n'+'\n'.join(lines[-10:]))
PY
