#!/usr/bin/env bash
set -Eeuo pipefail
python3 - "${1:?Usage: m2-report-isolation-evidence.sh retained-directory}" <<'PY'
import json,sys
from pathlib import Path
root=Path(sys.argv[1])
for name in ('evidence-a','evidence-b','evidence-c','evidence-d','evidence-e'):
    print('\n'+name)
    path=root/(name+'.json')
    if path.exists():
        machine=json.loads(path.read_text())[0]
        print(json.dumps({key:machine.get(key) for key in ('name','state','network')},separators=(',',':')))
    for suffix in ('-start.log','-host.log','-to-mac.jsonl'):
        path=root/(name+suffix)
        if path.exists():
            print(path.name+':')
            print('\n'.join(path.read_text(errors='replace').splitlines()[-12:]))
print('\nRetained native/guest diagnostics:')
for path in sorted((root/'retained-state').rglob('*.log')):
    print(str(path.relative_to(root))+':')
    lines=path.read_text(errors='replace').splitlines()
    relevant=[line for line in lines if any(word in line.lower() for word in ('error','failed','dhcp','offered','leased','route','isolation','interface','network ready'))]
    print('\n'.join(relevant[-10:]))
PY
