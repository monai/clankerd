#!/usr/bin/env python3
"""Stream a native gate's evidence, report quiet progress, and bound its runtime."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def run(log, command, timeout=1800, heartbeat=30, grace=30):
    started = time.monotonic()
    next_heartbeat = started + heartbeat
    with log.open('wb') as output, log.open('rb') as reader:
        # Keep the terminal session used by sudo -v, while isolating signals
        # for timeout cleanup. setsid would discard that authorization context.
        process = subprocess.Popen(command, stdout=output, stderr=subprocess.STDOUT, process_group=0)
        try:
            while True:
                data = reader.read()
                if data:
                    sys.stdout.buffer.write(data)
                    sys.stdout.buffer.flush()
                status = process.poll()
                if status is not None:
                    # The process may have written between the read and poll.
                    sys.stdout.buffer.write(reader.read())
                    sys.stdout.buffer.flush()
                    return status
                now = time.monotonic()
                if now - started >= timeout:
                    print(f'TIMEOUT after {timeout:g}s: {command}; evidence: {log}', flush=True)
                    return 124
                if now >= next_heartbeat:
                    print(f'STILL RUNNING ({int(now-started)}s): {command}; log: {log}', flush=True)
                    next_heartbeat = now + heartbeat
                time.sleep(.1)
        finally:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=grace)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                sys.stdout.buffer.write(reader.read())
                sys.stdout.buffer.flush()


if __name__ == '__main__':
    sys.exit(run(Path(sys.argv[1]), sys.argv[2:],
                 timeout=float(os.environ.get('ACCEPTANCE_GATE_TIMEOUT', '1800'))))
