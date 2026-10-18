#!/usr/bin/env python3
"""Require the recorded network PID to stop; other machines remain independent."""
import subprocess
import sys
import time


def verify(pid, timeout=5):
    if pid <= 1:
        raise ValueError('expected a recorded network PID greater than 1')
    deadline = time.monotonic() + timeout
    while True:
        process = subprocess.run(['/bin/ps', '-ww', '-p', str(pid), '-o', 'pid=,uid=,stat=,command='],
                                 text=True, capture_output=True)
        if process.returncode not in (0, 1):
            raise RuntimeError('cannot inspect network process: ' + process.stderr.strip())
        rows = [line.split(None, 3) for line in process.stdout.splitlines() if line.strip()]
        if not rows or all(row[2].startswith('Z') for row in rows):
            return
        if time.monotonic() >= deadline:
            raise RuntimeError('network sidecar outlived its machine: ' + process.stdout.strip())
        time.sleep(.1)


if __name__ == '__main__':
    try:
        verify(int(sys.argv[1]))
    except (ValueError, RuntimeError) as error:
        sys.exit('FAIL: ' + str(error))
    print('PASS removed machine network sidecar stopped (PID ' + sys.argv[1] + ')')
