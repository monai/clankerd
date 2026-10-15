#!/usr/bin/env bash
set -Eeuo pipefail
python3 - "${1:?Usage: m2-report-other-vmm.sh retained-directory}" <<'PY'
from pathlib import Path
import json,re,sys
root=Path(sys.argv[1])
assert root.is_dir(),root
for name in ('qemu-version.txt','qemu-netdevs.txt','firmware.json','helper.log','qemu-console.log','guest-network-before-provision.txt','guest-cloud-init.log','provision-timeout.log'):
    path=root/name
    if path.is_file():
        print('\n'+name+':')
        text=path.read_text(errors='replace')
        text=re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]','',text)
        lines=text.replace('\r','\n').splitlines()
        if name=='qemu-console.log':
            selected=[line for line in lines if any(word in line.lower() for word in ('ci-info','mgmt0','eth0','enp0','netplan','renam','network','ssh','failed','error','10.0.2.','192.168.'))]
            print('Network/boot evidence:\n'+'\n'.join(selected[-110:]))
            print('Console end:\n'+'\n'.join(lines[-25:]))
        else:
            print('\n'.join(lines[-70:] if name.startswith('guest-') else lines[-20:]))
print('\nSeed files:')
for path in sorted(root.rglob('*')):
    if path.is_file() and path.name in ('user-data','meta-data','network-config'):
        print(str(path.relative_to(root))+':')
        text=path.read_text(errors='replace')
        if path.name=='user-data':
            try:
                data=json.loads(text.split('\n',1)[1])
                for entry in data.get('write_files',[]):
                    if entry.get('encoding')=='b64':entry['content']='[embedded fixture omitted]'
                text=json.dumps(data,indent=2)
            except (ValueError,IndexError):pass
        text=re.sub(r'ssh-ed25519\s+\S+', 'ssh-ed25519 [public key omitted]',text)
        print(text[:10000])
path=root/'commands.log'
if path.is_file():
    lines=path.read_text(errors='replace').splitlines()
    setup=[line for line in lines if any(word in line for word in ('qemu-img','hdiutil','ssh-keygen'))]
    print('\nSetup commands:\n'+'\n'.join(setup))
    attempts=[line for line in lines if 'ssh ' in line or 'banner exchange' in line or 'timed out' in line]
    print('\nLast management attempt:\n'+'\n'.join(attempts[-4:]))
PY
