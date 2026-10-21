#!/usr/bin/env python3
"""Native vmctl acceptance: shell-free and conflicting glibc workload filesystems."""
import importlib.util
import json
import os
import signal
from pathlib import Path
import subprocess
import sys
import tempfile
import time

bundle, probe, evidence = map(lambda p: Path(p).resolve(), sys.argv[1:])
root = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('registry', root / 'fixtures/management-registry.py')
registry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(registry)
work = Path(tempfile.mkdtemp(prefix='clankerd-mgmt-', dir='/tmp'))
env = dict(os.environ, CLANKERD_STATE_DIR=str(work / 'state'), CLANKERD_RUNTIME_DIR=str(work / 'run'))
vmctl = bundle / 'darwin-arm64/vmctl'
log = (evidence / 'management-commands.log').open('w')
server, image = registry.start(evidence, probe, log)
registry_address = image.split('/')[0]
sequence = 0

def vm(*args, check=True):
    global sequence
    sequence += 1
    command = [str(vmctl), '--insecure-registry', registry_address, *map(str, args)]
    print('vmctl', *args, flush=True)
    log.write(json.dumps(command) + '\n'); log.flush()
    result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=600)
    (evidence / f'management-{sequence:03d}.log').write_text(result.stdout + result.stderr)
    if check and result.returncode:
        raise RuntimeError(f'{args}: {result.returncode}: {result.stderr}')
    return result.stdout


def observe(machine, path):
    value = json.loads(vm('exec', machine, path))
    assert value['same_network_namespace'] and value['different_mount_namespace'], value
    assert 'clankerd-guestd' in value['management_pid1_exe'], value
    assert '4242:4242' not in value['management_passwd'], value
    deadline = time.monotonic() + 30
    while not any(line.startswith('nameserver ') for line in value['resolver'].splitlines()):
        if time.monotonic() > deadline:
            raise AssertionError('shared resolver not published')
        time.sleep(.25)
        value = json.loads(vm('exec', machine, path))
    assert value['resolver'] == value['management_resolver'], value
    workers = [p for p in value['processes'] if 'dhcpcd' in p['cmdline']]
    assert len(workers) >= 3, workers
    assert any('\nSeccomp:\t2' in p['status'] and '\nCapEff:\t0000000000000000' in p['status']
               and '\nUid:\t0\t' not in p['status'] for p in workers), workers
    assert '\nCapEff:\t0000000000000000' not in value['status'], value['status']
    (evidence / f'{machine}-observation.json').write_text(json.dumps(value, indent=2) + '\n')
    # The actual VMM must remain owned by this user, with installed libraries.
    machine_info = json.loads(vm('inspect', machine))[0]
    (evidence / f'{machine}-inspect.json').write_text(json.dumps(machine_info, indent=2) + '\n')
    subprocess.run([sys.executable, str(root / 'accept-vm-process.py'),
                    str(machine_info['state']['pid']), str(bundle), str(evidence)], check=True)
    return value

try:
    vm('run', '-d', '--name', 'management-scratch', image)
    value = observe('management-scratch', '/probe')
    assert value['shell_absent'] and '4242:4242' in value['workload_passwd'], value
    vm('exec', 'management-scratch', '/probe', 'mount')
    vm('exec', 'management-scratch', '/probe', 'mount-visible')
    # Revoke sudo credentials while the live authorization session owns the
    # attachment. Its replacement must succeed without a new password prompt.
    subprocess.run(['sudo', '-k'], check=True)
    machine = json.loads(vm('inspect', 'management-scratch'))[0]
    pidfile = work / 'state/machines' / machine['id'] / 'net.pid'
    previous_pid = int(pidfile.read_text())
    os.kill(previous_pid, signal.SIGKILL)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if pidfile.exists() and int(pidfile.read_text()) != previous_pid:
            observe('management-scratch', '/probe')
            break
        time.sleep(.25)
    else:
        raise AssertionError('native attachment did not recover after sudo credentials were revoked')

    for _ in range(2):
        vm('stop', 'management-scratch'); vm('start', 'management-scratch')
        observe('management-scratch', '/probe')
    vm('run', '-d', '--name', 'management-glibc', '-v', str(probe.parent) + ':/fixture:ro',
       os.environ.get('GLIBC_IMAGE', 'debian:bookworm-slim'), 'sleep', 'infinity')
    assert 'glibc' in vm('exec', 'management-glibc', 'getconf', 'GNU_LIBC_VERSION').lower()
    vm('exec', 'management-glibc', 'sh', '-c',
       'printf "dhcpcd:x:4242:4242:hostile:/missing:/missing\\n" >> /etc/passwd; '
       'printf broken > /lib/ld-musl-aarch64.so.1; printf "noipv4\\nnoipv6\\n" > /etc/dhcpcd.conf; '
       'mkdir -p /var/lib/dhcpcd /run/dhcpcd; echo hostile > /var/lib/dhcpcd/workload-only; '
       'echo ephemeral > /run/workload-only')
    observe('management-glibc', '/fixture/' + probe.name)
    assert vm('exec', '-u', '1234:1234', '-w', '/tmp', 'management-glibc', 'sh', '-c', 'id -u; pwd').strip() == '1234\n/tmp'
    vm('stop', 'management-glibc'); vm('start', 'management-glibc')
    observe('management-glibc', '/fixture/' + probe.name)
    vm('exec', 'management-glibc', 'sh', '-ec',
       'test ! -e /run/workload-only; test ! -e /var/run/workload-only; '
       'test "$(cat /var/lib/dhcpcd/workload-only)" = hostile; '
       'echo current > /run/workload-only; test "$(cat /var/run/workload-only)" = current')
    (evidence / 'management-images.json').write_text(vm('image', 'ls', '--output', 'json'))
    (evidence / 'management-result.json').write_text(json.dumps({'status': 'PASS', 'state': str(work)}) + '\n')
except BaseException as error:
    (evidence / 'management-result.json').write_text(json.dumps({'status': 'FAIL', 'error': str(error), 'state': str(work)}) + '\n')
    raise
finally:
    for name in ['management-scratch', 'management-glibc']:
        vm('rm', '-f', name, check=False)
    server.shutdown()
    log.close()
    print('Management evidence:', evidence, 'state:', work)
