"""Report missing Homebrew dependencies reachable through Mach-O load commands."""
from pathlib import Path
import subprocess
import sys

pending = [Path(sys.argv[1])]
seen = set()
formulas = set()
while pending:
    path = pending.pop()
    resolved = path.resolve()
    if resolved in seen:
        continue
    seen.add(resolved)
    output = subprocess.check_output(['/usr/bin/otool', '-L', str(path)], text=True)
    for line in output.splitlines()[1:]:
        dependency = line.strip().split(' (', 1)[0]
        if not dependency.startswith('/opt/homebrew/'):
            continue
        target = Path(dependency)
        if not target.is_file():
            print('Missing linked dependency: ' + dependency, file=sys.stderr)
            parts = target.parts
            if len(parts) > 4 and parts[3] in ('opt', 'Cellar'):
                formulas.add(parts[4])
            else:
                raise SystemExit('No formula identified for ' + dependency)
        else:
            pending.append(target)
print('\n'.join(sorted(formulas)))
