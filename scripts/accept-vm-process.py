#!/usr/bin/env python3
"""Verify the machine's recorded VMM PID rather than matching ps command text."""
import os
import re
from pathlib import Path
import subprocess
import sys


def verify(pid, bundle, evidence):
    assert isinstance(pid, int) and pid > 0, pid
    process = subprocess.check_output(
        ['/bin/ps', '-p', str(pid), '-o', 'pid=,uid='], text=True)
    (evidence / f'process-{pid}.log').write_text(process)
    rows = [line.split() for line in process.splitlines() if line.strip()]
    assert rows == [[str(pid), str(os.getuid())]] and os.getuid() != 0, rows
    mappings = subprocess.check_output(
        ['/usr/bin/vmmap', '-w', str(pid)], text=True, stderr=subprocess.STDOUT)
    (evidence / f'vmmap-{pid}.log').write_text(mappings)
    assert str(bundle / 'darwin-arm64/clankerd-vmspawn') in mappings, mappings
    assert re.search(r'/libkrun(?:\.\d+)*\.dylib(?:\s|$)', mappings), mappings


if __name__ == '__main__':
    verify(int(sys.argv[1]), Path(sys.argv[2]).resolve(), Path(sys.argv[3]).resolve())
