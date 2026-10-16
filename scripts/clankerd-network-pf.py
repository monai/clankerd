#!/usr/bin/python3 -I
"""Own only com.apple/clankerd-networks; the Mac administrator controls host policy."""
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

ANCHOR = 'com.apple/clankerd-networks'
STATE = Path('/var/run/clankerd-network-pf')


def command(args, text=None):
    result = subprocess.run(args, input=text, text=True, capture_output=True, timeout=15)
    if result.returncode:
        raise RuntimeError(' '.join(args) + ': ' + result.stderr.strip())
    return result.stdout + result.stderr


def alive(record):
    try:
        output = command(['/bin/ps', '-o', 'uid=,command=', '-p', str(record['pid'])])
        fields = output.split()
        return (int(fields[0]) == record['uid'] and fields[1].endswith('/vmnet-helper')
                and any(fields[i:i + 2] == ['--socket', record['socket']]
                        for i in range(2, len(fields) - 1)))
    except (RuntimeError, IndexError, ValueError):
        return False


def rules(records):
    bridges = sorted({row['bridge'] for row in records.values()})
    lines = []
    for bridge in bridges:
        lines.append(f'anchor "mark-{bridge}" in on {bridge} all tag "clankerd_{bridge}"')
    for source in bridges:
        for target in bridges:
            if source != target:
                lines.append(f'block drop out quick on {target} all tagged "clankerd_{source}" '
                             f'label "clankerd-{source}-{target}"')
    return '\n'.join(lines) + '\n'


def bridge_for(subnet):
    gateway = str(subnet.network_address + 1)
    for attempt in range(20):
        output = command(['/sbin/route', '-n', 'get', gateway])
        match = re.search(r'^\s*interface:\s*(bridge[0-9]+)\s*$', output, re.M)
        if match:
            bridge = match[1]
            addresses = command(['/sbin/ifconfig', bridge])
            if re.search(r'\binet ' + re.escape(gateway) + r'\s', addresses):
                return bridge
        time.sleep(0.1)
    raise RuntimeError(f'no native bridge owns gateway {gateway}')


def validate(request, uid):
    if request.get('action') not in ('register', 'remove'):
        raise ValueError('action must be register or remove')
    socket = request['socket']
    if not isinstance(socket, str) or not socket.startswith('/') or len(socket) > 1024:
        raise ValueError('invalid absolute socket path')
    key = hashlib.sha256(f'{uid}:{socket}'.encode()).hexdigest()
    if request['action'] == 'remove':
        return key, None
    subnet = ipaddress.IPv4Network(request['subnet'])
    private = [ipaddress.ip_network(n) for n in ('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16')]
    if not 8 <= subnet.prefixlen <= 30 or not any(subnet.subnet_of(n) for n in private):
        raise ValueError('expected aligned RFC1918 subnet /8 through /30')
    network_id = request.get('network_id')
    if not isinstance(network_id, str) or not re.fullmatch(r'[0-9a-f]{64}', network_id):
        raise ValueError('registration requires a stable virtual network identity')
    prefix = request.get('prefix')
    if prefix is not None:
        prefix = str(ipaddress.IPv6Network(prefix + '/64'))
        if not ipaddress.ip_network(prefix).subnet_of(ipaddress.ip_network('fd00::/8')):
            raise ValueError('expected aligned ULA /64')
    record = dict(uid=uid, socket=socket, pid=int(request['pid']), network_id=network_id,
                  subnet=str(subnet), prefix=prefix,
                  bridge=bridge_for(subnet))
    if record['pid'] <= 1 or not alive(record):
        raise ValueError('attachment helper does not belong to caller and socket')
    return key, record


def apply(previous, desired, token, persist):
    content = rules(desired)
    command(['/sbin/pfctl', '-n', '-a', ANCHOR, '-f', '-'], content)
    acquired = False
    if desired and not token:
        output = command(['/sbin/pfctl', '-E'])
        match = re.search(r'Token\s*:\s*(\d+)', output)
        if not match:
            raise RuntimeError('pfctl did not return an enable reference token')
        token = match[1]
        acquired = True
    try:
        command(['/sbin/pfctl', '-a', ANCHOR, '-f', '-'], content)
        persist(token if desired else None)
    except Exception:
        try:
            command(['/sbin/pfctl', '-a', ANCHOR, '-f', '-'], rules(previous))
        finally:
            if acquired:
                command(['/sbin/pfctl', '-X', token])
        raise
    if not desired and token:
        try:
            command(['/sbin/pfctl', '-X', token])
        except Exception:
            persist(token)
            raise
        token = None
    return token


def main():
    if sys.platform != 'darwin' or os.geteuid() != 0:
        raise RuntimeError('requires root on macOS')
    uid = int(os.environ.get('SUDO_UID', os.getuid()))
    request = json.loads(sys.stdin.read(4096))
    STATE.mkdir(mode=0o700, exist_ok=True)
    with (STATE / 'lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = STATE / 'state.json'
        state = json.loads(path.read_text()) if path.exists() else {'records': {}, 'token': None}
        previous = state['records']
        desired = {key: row for key, row in previous.items() if alive(row)}
        key, record = validate(request, uid)
        if record:
            for other_key, other in desired.items():
                if other_key == key:
                    continue
                same_network = other.get('network_id') == record['network_id']
                same_bridge = other['bridge'] == record['bridge']
                same_addresses = (other['subnet'], other['prefix']) == (record['subnet'], record['prefix'])
                if same_network != same_bridge or (same_bridge and not same_addresses):
                    raise ValueError('native bridge conflicts with an active virtual network')
            desired[key] = record
        else:
            desired.pop(key, None)
        if desired:
            root_rules = command(['/sbin/pfctl', '-sr'])
            if not re.search(r'^anchor "com\.apple/\*"', root_rules, re.M):
                raise RuntimeError('host PF rules must include anchor "com.apple/*"')
        def persist(token):
            temporary = STATE / 'state.tmp'
            temporary.write_text(json.dumps({'records': desired, 'token': token}, sort_keys=True))
            temporary.replace(path)
        apply(previous, desired, state['token'], persist)
        print(json.dumps({'attachments': len(desired), 'bridges': sorted({r['bridge'] for r in desired.values()})}))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print('clankerd network PF: ' + str(error), file=sys.stderr)
        sys.exit(1)
