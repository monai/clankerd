#!/usr/bin/env bash
# Summarize retained recovery diagnostics without starting a VM.
set -euo pipefail
python3 - "$@" <<'PY'
from pathlib import Path
import sys

if len(sys.argv) > 1:
    directory = Path(sys.argv[1])
else:
    candidates = list(Path('/tmp').glob('clankerd-recovery.*'))
    if not candidates:
        raise SystemExit('No retained recovery diagnostics found')
    directory = max(candidates, key=lambda path: path.stat().st_mtime)
if not directory.is_dir():
    raise SystemExit(f'Diagnostics directory not found: {directory}')
print(f'Recovery report: {directory}')
baseline_path = directory / 'native-count-baseline-idle'
baseline = int(baseline_path.read_text()) if baseline_path.exists() else None
print(f'Idle baseline: {baseline if baseline is not None else "not captured"}')
counts = sorted(directory.glob('native-count-*'), key=lambda path: path.stat().st_mtime)
for path in counts:
    tag = path.name.removeprefix('native-count-')
    if 'idle' not in tag and tag not in ('startup-busy', 'burst-busy', 'failure'):
        continue
    count = int(path.read_text())
    delta = f' ({count - baseline:+d})' if baseline is not None else ''
    print(f'{tag}: {count}{delta}')
    if tag.startswith('management-'):
        guest = directory / f'guest-{tag.removesuffix("-idle")}.txt'
        if guest.exists():
            print('  ' + guest.read_text().strip()[:200])
for filename in ('check.log', 'startup.log'):
    path = directory / filename
    if path.exists() and path.stat().st_size:
        print(f'{filename}:')
        for line in path.read_text(errors='replace').splitlines()[-5:]:
            print('  ' + line[:240])
print('Latest machine state:')
import json
path = directory / 'current.json'
if path.exists():
    try:
        machine = json.loads(path.read_text())[0]
        print(json.dumps(machine.get('state'), separators=(',', ':')))
    except (ValueError, IndexError, KeyError):
        print('  Latest inspection could not be decoded')
else:
    print('  Not captured')
print('Full socket details remain in the retained directory.')
PY
