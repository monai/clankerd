#!/usr/bin/env python3
"""Minimise native interface creation failure; does not claim packet isolation."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import uuid

helper = os.environ.get('CLANKERD_VMNET_HELPER', '/opt/vmnet-helper/bin/vmnet-helper')
work = Path(tempfile.mkdtemp(prefix='vmnet-isolation-', dir='/tmp'))
children = []
results = []


def helper_pid(socket):
    output = subprocess.run(['ps', '-axo', 'pid=,uid=,command='], capture_output=True, text=True, check=True).stdout
    for line in output.splitlines():
        fields = line.split()
        if len(fields) < 4 or fields[1] != str(os.getuid()) or fields[2] != helper:
            continue
        if any(fields[i] == '--socket' and fields[i + 1] == str(socket) for i in range(3, len(fields) - 1)):
            return int(fields[0])


def stop(item):
    process, socket = item
    if process.poll() is None:
        pid = helper_pid(socket)
        if pid is not None:
            os.kill(pid, signal.SIGTERM)
        else:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except PermissionError:
                # Only the child owned by this diagnostic is targeted. This
                # fallback needs an existing sudo credential for a hung startup.
                subprocess.run(['sudo', '--non-interactive', '--', '/bin/kill', '-TERM', str(process.pid)], check=False)
        try:
            process.wait(timeout=4)
        except subprocess.TimeoutExpired:
            raise RuntimeError(f'Cannot stop owned helper startup PID {process.pid}; diagnostics retained in {work}')
    try:
        socket.unlink()
    except FileNotFoundError:
        pass


def stop_all():
    while children:
        stop(children.pop())


def launch(label, mode='shared', isolated=False, subnet='192.168.105', identity=True):
    socket = work / f'{label}.sock'
    output = work / f'{label}.json'
    log = work / f'{label}.log'
    args = ['sudo', '--non-interactive', '--', helper, '--socket', str(socket), '--operation-mode', mode]
    if identity:
        args += ['--interface-id', str(uuid.uuid4())]
    if subnet:
        args += ['--start-address', subnet + '.1', '--end-address', subnet + '.254', '--subnet-mask', '255.255.255.0']
    if isolated:
        args += ['--enable-isolation']
    print(f'CASE {label}: {json.dumps(args)}', flush=True)
    with output.open('w') as stdout, log.open('w') as stderr:
        process = subprocess.Popen(args, stdout=stdout, stderr=stderr, stdin=subprocess.DEVNULL, start_new_session=True)
    item = (process, socket)
    children.append(item)
    deadline = time.monotonic() + 8
    metadata = None
    while time.monotonic() < deadline and process.poll() is None:
        for line in output.read_text().splitlines():
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if isinstance(value, dict) and value.get('vmnet_mac_address') and value.get('vmnet_mtu'):
                metadata = value
        if metadata and socket.exists():
            break
        time.sleep(.05)
    status = 'ready' if metadata and socket.exists() and process.poll() is None else 'failed'
    result = dict(case=label, mode=mode, isolated=isolated, subnet=subnet, identity=identity,
                  status=status, exit_code=process.poll(), metadata=metadata)
    results.append(result)
    print(json.dumps(result, sort_keys=True), flush=True)
    print(log.read_text(), flush=True)
    return result


def main():
    try:
        # Same exact selected subnet, without a competing interface.
        launch('shared-alone-control')
        stop_all()
        launch('isolated-alone-explicit', isolated=True)
        stop_all()
        launch('isolated-alone-no-uuid', isolated=True, identity=False)
        stop_all()
        launch('isolated-alone-automatic', isolated=True, subnet=None)
        stop_all()
        # Host mode is a diagnostic control, never a replacement for shared NAT.
        launch('host-isolated-control', mode='host', isolated=True, subnet='10.251.14')
        stop_all()
        # Reproduce exact ordering: two participating peers, then an isolated
        # interface using the same selected network. No VM/MAC is involved.
        launch('shared-peer-a')
        launch('shared-peer-b')
        mixed = launch('same-subnet-isolated', isolated=True)
        launch('different-subnet-isolated', isolated=True, subnet='10.251.12')
        stop_all()
        # Fresh subnet excludes conflicts involving the historical default.
        launch('fresh-peer-a', subnet='10.251.11')
        launch('fresh-peer-b', subnet='10.251.11')
        launch('fresh-same-subnet-isolated', isolated=True, subnet='10.251.11')
        stop_all()
        # Reverse creation order tests whether isolation is part of native
        # network compatibility, rather than an interface-local setting.
        launch('isolation-first', isolated=True, subnet='10.251.13')
        launch('isolation-first-plain-peer', subnet='10.251.13')
        stop_all()
        (work / 'summary.json').write_text(json.dumps(results, indent=2))
        print(f'Diagnostic summary: {work / "summary.json"}', flush=True)
        print('This matrix tests interface creation only; it establishes no IPv4/IPv6 packet-policy fallback.', flush=True)
        return 1 if mixed['status'] == 'failed' else 0
    finally:
        stop_all()


if __name__ == '__main__':
    sys.exit(main())
