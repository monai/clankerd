#!/usr/bin/env python3
"""Owned QEMU/Ubuntu guest: actual other-VMM packets and Avahi discovery.

QEMU dgram sends raw Ethernet: QEMU net/dgram.c (v9.2+).
Helper socket protocol: nirs/vmnet-helper docs/integration.md.
Ubuntu image provenance: cloud-images.ubuntu.com/noble/current/SHA256SUMS.
NoCloud CIDATA: docs.cloud-init.io/reference/datasources/nocloud.html.
"""
import base64
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid

ROOT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[2]
WORK = Path(tempfile.mkdtemp(prefix='clankerd-other-vmm-', dir='/tmp'))
ENV = dict(os.environ, CLANKERD_STATE_DIR=str(WORK/'state'), CLANKERD_RUNTIME_DIR=str(WORK/'run'))
VMCTL = str(Path(os.environ.get('OUT', ROOT/'build/rust'))/'darwin-arm64/vmctl')
PROCESSES = []
LOGS = []
MACHINES = []


def run(args, *, timeout=60, check=True, input=None):
    result = subprocess.run([str(a) for a in args], input=input, text=True,
                            capture_output=True, env=ENV, timeout=timeout)
    with (WORK/'commands.log').open('a') as log:
        log.write(shlex.join(map(str, args))+'\n'+result.stdout+result.stderr+'\n')
    if check and result.returncode:
        raise RuntimeError(f'{args[0]} failed ({result.returncode}): {result.stderr[-2000:]}')
    return result


def start(args, log):
    output = (WORK/log).open('w'); LOGS.append(output)
    process = subprocess.Popen([str(a) for a in args], stdout=output, stderr=subprocess.STDOUT, env=ENV)
    PROCESSES.append(process)
    return process


def progress(message):
    print(message, flush=True)


def until(action, seconds, description):
    deadline = time.monotonic()+seconds
    last = None
    heartbeat = time.monotonic()
    while time.monotonic() < deadline:
        try:
            value = action()
            if value: return value
        except (OSError, RuntimeError, ValueError, AssertionError, subprocess.TimeoutExpired) as error:
            last = error
        if time.monotonic()-heartbeat >= 20:
            progress('Waiting: '+description)
            heartbeat=time.monotonic()
        time.sleep(2)
    raise RuntimeError(f'Timeout: {description}: {last}')


def image():
    base = 'https://cloud-images.ubuntu.com/noble/current/'
    filename = 'noble-server-cloudimg-arm64.img'
    progress('Download Ubuntu checksum manifest and disposable guest image')
    manifest = urllib.request.urlopen(base+'SHA256SUMS', timeout=60).read()
    (WORK/'SHA256SUMS').write_bytes(manifest)
    (WORK/'image-source.json').write_text(json.dumps({'url':base+filename,'manifest':base+'SHA256SUMS'}))
    expected = next(line.split()[0] for line in manifest.decode().splitlines()
                    if line.split()[-1].lstrip('*') == filename)
    cache = Path(tempfile.gettempdir())/('clankerd-qemu-image-cache-'+str(os.getuid()))
    cache.mkdir(mode=0o700,exist_ok=True)
    destination = cache/(expected+'-'+filename)
    if destination.is_file():
        digest=hashlib.sha256()
        with destination.open('rb') as source:
            for block in iter(lambda:source.read(1024*1024),b''):digest.update(block)
        if digest.hexdigest()==expected:
            progress('Reuse checksum-verified Ubuntu image from test-owned cache')
            return destination
        destination.unlink()
    with urllib.request.urlopen(base+filename, timeout=90) as response, destination.open('wb') as output:
        downloaded=0;deadline=time.monotonic()+600;report=0
        while block:=response.read(1024*1024):
            assert time.monotonic()<deadline and downloaded<3*1024**3,'Ubuntu image download deadline/size exceeded'
            output.write(block);downloaded+=len(block)
            if downloaded-report>=64*1024**2:
                progress(f'Ubuntu image: {downloaded//1024**2} MiB');report=downloaded
    digest = hashlib.sha256()
    with destination.open('rb') as source:
        for block in iter(lambda:source.read(1024*1024), b''):digest.update(block)
    assert digest.hexdigest() == expected, 'Ubuntu image checksum mismatch'
    (WORK/'image-sha256.txt').write_text(expected+'  '+filename+'\n')
    return destination


def firmware(qemu):
    prefix = Path(run(['brew','--prefix','qemu']).stdout.strip())
    for folder in (prefix/'share/qemu/firmware', Path('/opt/homebrew/share/qemu/firmware')):
        for config in sorted(folder.glob('*.json')):
            data = json.loads(config.read_text())
            if not any(t.get('architecture') == 'aarch64' for t in data.get('targets', [])):continue
            executable = data.get('mapping',{}).get('executable',{}).get('filename')
            if executable and Path(executable).is_file():
                (WORK/'firmware.json').write_text(json.dumps(data, indent=2))
                return executable
    candidate = prefix/'share/qemu/edk2-aarch64-code.fd'
    assert candidate.is_file(), 'No installed AArch64 UEFI firmware found'
    return str(candidate)


def helper():
    binary = os.environ.get('CLANKERD_VMNET_HELPER','/opt/vmnet-helper/bin/vmnet-helper')
    args = ['sudo','--non-interactive','--',binary,'--socket',str(WORK/'helper.sock'),
            '--operation-mode','shared','--interface-id',str(uuid.uuid4()),
            '--start-address','192.168.105.1','--end-address','192.168.105.254',
            '--subnet-mask','255.255.255.0']
    process = start(args,'helper.log')
    def ready():
        assert process.poll() is None, 'helper exited'
        for line in (WORK/'helper.log').read_text().splitlines():
            try: info=json.loads(line)
            except ValueError:continue
            if info.get('vmnet_mac_address') and (WORK/'helper.sock').exists():return info
    return until(ready,30,'native helper JSON and socket')


def seed(mac, server, ssh_public):
    directory=WORK/'seed';directory.mkdir()
    (directory/'meta-data').write_text('instance-id: '+str(uuid.uuid4())+'\nlocal-hostname: interop-qemu\n')
    (directory/'network-config').write_text('''version: 2
ethernets:
  mgmt0:
    match: {macaddress: "52:54:00:71:00:01"}
    set-name: mgmt0
    dhcp4: false
    addresses: [10.0.2.15/24]
    nameservers: {addresses: [10.0.2.3]}
    routes:
      - {to: default, via: 10.0.2.2, metric: 900}
  eth0:
    match: {macaddress: "'''+mac+'''"}
    set-name: eth0
    dhcp4: true
    dhcp6: true
    accept-ra: true
    dhcp4-overrides: {route-metric: 50}
''')
    configuration = {'users':[{'name':'gate','sudo':'ALL=(ALL) NOPASSWD:ALL','shell':'/bin/bash',
                              'ssh_authorized_keys':[ssh_public.strip()]}],
                     'write_files':[{'path':'/opt/echo.py','encoding':'b64','content':base64.b64encode(server.encode()).decode()},
                                    {'path':'/etc/avahi/avahi-daemon.conf','content':'[server]\nhost-name=interop-qemu\nallow-interfaces=eth0\nuse-ipv4=yes\nuse-ipv6=yes\n[reflector]\nenable-reflector=no\n'}],
                     'runcmd':[['touch','/var/lib/interop-seed-ready']]}
    configuration['write_files'].extend([
        {'path':'/etc/systemd/network/05-interop-management.network','content':'[Match]\nMACAddress=52:54:00:71:00:01\n[Link]\nRequiredForOnline=no\n[Network]\nAddress=10.0.2.15/24\nDNS=10.0.2.3\n[Route]\nGateway=10.0.2.2\nMetric=900\n'},
        {'path':'/etc/systemd/network/05-interop-test.network','content':'[Match]\nMACAddress='+mac+'\n[Link]\nRequiredForOnline=no\n[Network]\nDHCP=yes\nIPv6AcceptRA=yes\n[DHCPv4]\nRouteMetric=50\n'},
        {'path':'/opt/interop-network.sh','permissions':'0755','content':'#!/bin/sh\nset -eux\nsystemctl restart systemd-networkd\nip link set mgmt0 up\nip address replace 10.0.2.15/24 dev mgmt0\nip route replace default via 10.0.2.2 dev mgmt0 metric 900\nip link set eth0 up\nresolvectl dns mgmt0 10.0.2.3\nresolvectl domain mgmt0 "~."\nip -brief address\nip route\nnetworkctl status --no-pager mgmt0 eth0\n'}
    ])
    configuration['runcmd'].insert(0,['/opt/interop-network.sh'])
    (directory/'user-data').write_text('#cloud-config\n'+json.dumps(configuration))
    run(['hdiutil','makehybrid','-iso','-joliet','-default-volume-name','CIDATA','-o',WORK/'seed.iso',directory])


def ssh(command, *, timeout=30, check=True):
    return run(['ssh','-F','/dev/null','-i',WORK/'key','-o','BatchMode=yes','-o','ConnectTimeout=3',
                '-o','StrictHostKeyChecking=accept-new','-o',f'UserKnownHostsFile={WORK}/known_hosts',
                '-p',str(SSH_PORT),'gate@127.0.0.1',command],timeout=timeout,check=check).stdout


def guest(name, command, timeout=30):
    if name=='qemu':return ssh(command,timeout=timeout)
    return run([VMCTL,'exec',name,'sh','-c',command],timeout=timeout).stdout


def addresses(name):
    output=guest(name,'ip -j address show dev eth0')
    (WORK/f'{name}-addresses.json').write_text(output)
    records=json.loads(output)[0]['addr_info']
    usable=[r['local'] for r in records if not any(r.get(f,False) for f in ('tentative','dadfailed','deprecated')) and r.get('preferred_life_time')!=0]
    result={}
    for address in usable:
        ip=ipaddress.ip_address(address)
        key='4' if ip.version==4 else ('ll6' if ip.is_link_local else '6')
        result.setdefault(key,address)
    assert all(k in result for k in ('4','6','ll6')), f'{name}: incomplete dual-stack addresses {result}'
    return result


def python_command(code,*args):
    return shlex.join(['python3','-c',code,*args])


def route_check(name,address):
    target=address.split('%')[0]
    output=guest(name,'ip -j route get '+shlex.quote(target)+(' oif eth0' if ipaddress.ip_address(target).is_link_local else ''))
    assert json.loads(output)[0]['dev']=='eth0', f'{name}: tested route uses management NIC: {output}'
    with (WORK/'test-routes.jsonl').open('a') as log:log.write(json.dumps({'source':name,'target':address,'route':json.loads(output)})+'\n')


def matrix(source,target,source_addresses,target_addresses,peer,expected):
    for family,address in target_addresses.items():
        scoped=address+'%eth0' if family=='ll6' else address
        route_check(source,scoped)
        source_ip=source_addresses[family]
        command='EVIDENCE_SOURCE='+shlex.quote(source_ip)+' '+python_command(peer,'64',scoped)
        rows=[json.loads(line) for line in guest(source,command).splitlines() if line.strip()]
        for row in rows:
            row.update(source=source,target=target,expected=expected)
            with (WORK/'matrix.jsonl').open('a') as output:output.write(json.dumps(row)+'\n')
        if not expected:
            assert all(any(reason in row.get('error','').lower() for reason in ('timed out','no route to host','host is unreachable')) for row in rows), f'Negative path failed locally or returned unexpected data: {rows}'
        assert len(rows)==2 and all(row['reached']==expected for row in rows), f'{source}->{target} {family}: {rows}'
        progress(f'PASS {source}->{target} {family} TCP/UDP '+('reachable' if expected else 'no delivery (positive controls passed)'))


def mac_control(name,items,probe):
    for family,address in items.items():
        if family=='ll6':
            def scoped():
                for index,interface in socket.if_nameindex():
                    if not interface.startswith('bridge'):continue
                    try:probe.probe(address,index);return True
                    except OSError:pass
            until(scoped,20,f'Mac->{name} scoped link-local TCP/UDP')
        else:until(lambda: probe.probe(address) or True,20,f'Mac->{name} {family} TCP/UDP')
    progress('PASS Mac->'+name+' IPv4/global6/link-local6 TCP/UDP')


def mdns(source,target,label,expected):
    # Real Avahi DNS-SD publisher; filter target interface, uncached random label.
    command='timeout 10 avahi-browse -rpt _clankerdgate._tcp'
    output=guest(source,command+' || test "$?" = 124',timeout=15)
    (WORK/f'{source}-browse-{target}.log').write_text(output)
    protocols={line.split(';')[2] for line in output.splitlines() if line.startswith('=;eth0;') and label in line}
    discovered=any(line.startswith(('+;eth0;','=;eth0;')) and label in line for line in output.splitlines())
    assert (protocols=={'IPv4','IPv6'} if expected else not discovered), f'Avahi {source}->{target}: resolved protocols={protocols}, expected={expected}'
    progress(f'PASS Avahi {source}->{target}: '+('resolved' if expected else 'not discovered'))


def main():
    global SSH_PORT
    progress('Evidence directory: '+str(WORK))
    for executable in ('qemu-system-aarch64','qemu-img','hdiutil','ssh','ssh-keygen','brew'):
        assert shutil.which(executable), 'Missing prerequisite '+executable
    qemu=shutil.which('qemu-system-aarch64')
    backend_result=run([qemu,'-machine','virt','-netdev','help'],check=False)
    backends=backend_result.stdout+backend_result.stderr
    (WORK/'qemu-netdevs.txt').write_text(backends)
    assert 'dgram' in backends, 'Installed QEMU lacks raw UNIX datagram backend'
    (WORK/'qemu-version.txt').write_text(run([qemu,'--version']).stdout)
    assert 'hvf' in run([qemu,'-machine','virt','-accel','help']).stdout, 'Installed QEMU lacks HVF'
    uefi=firmware(qemu)
    disk=image()
    run(['qemu-img','create','-f','qcow2','-F','qcow2','-b',disk,WORK/'overlay.qcow2','12G'])
    run(['ssh-keygen','-q','-t','ed25519','-N','','-f',WORK/'key'])
    server=(ROOT/'scripts/fixtures/vmnet-ipv6-services.py').read_text()
    peer=(ROOT/'scripts/fixtures/vmnet-isolation-peers.py').read_text().replace('connection.bind((source, 0))', 'connection.bind((source, 0, 0, scope) if family == socket.AF_INET6 else (source, 0))')
    info=helper();seed(info['vmnet_mac_address'],server,(WORK/'key.pub').read_text())
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1',0));SSH_PORT=reservation.getsockname()[1]
    process=start([qemu,'-machine','virt','-accel','hvf','-cpu','host','-smp','2','-m','2048',
        '-bios',uefi,'-display','none','-serial','stdio','-monitor','none',
        '-drive',f'file={WORK}/overlay.qcow2,format=qcow2,if=virtio',
        '-drive',f'file={WORK}/seed.iso,format=raw,if=virtio,readonly=on',
        '-netdev',f'user,id=management,net=10.0.2.0/24,host=10.0.2.2,dns=10.0.2.3,hostfwd=tcp:127.0.0.1:{SSH_PORT}-10.0.2.15:22',
        '-device','virtio-net-pci,netdev=management,mac=52:54:00:71:00:01',
        '-netdev',f'dgram,id=test,local.type=unix,local.path={WORK}/qemu.sock,remote.type=unix,remote.path={WORK}/helper.sock',
        '-device','virtio-net-pci,netdev=test,mac='+info['vmnet_mac_address']], 'qemu-console.log')
    progress('Boot QEMU: waiting for management SSH (bounded 4 minutes)')
    def ready():
        assert process.poll() is None,'QEMU exited; see console log'
        return ssh('echo management-ready',timeout=8).strip()=='management-ready'
    try:
        until(ready,240,'QEMU management SSH')
    except RuntimeError:
        console=(WORK/'qemu-console.log').read_text(errors='replace')
        relevant=[line for line in console.replace('\r','\n').splitlines() if any(word in line.lower() for word in ('ci-info','mgmt0','eth0','error','failed','interop-network','10.0.2.')) and 'start running' not in line]
        progress('QEMU boot failure evidence:\n'+'\n'.join(relevant[-65:]))
        raise
    progress('PASS management SSH; recording guest networking before provisioning')
    (WORK/'guest-cloud-init.log').write_text(ssh('sudo tail -150 /var/log/cloud-init.log; sudo journalctl -u systemd-networkd --no-pager -n 80',check=False))
    (WORK/'guest-network-before-provision.txt').write_text(ssh('ip -j address; ip route; ip -6 route; cat /etc/resolv.conf; resolvectl status; sudo cloud-init status --long',check=False))
    progress('Install fixture packages through management networking (bounded 5 minutes)')
    ssh('sudo env DEBIAN_FRONTEND=noninteractive sh -ec "apt-get update -qq; apt-get install -y -qq python3 avahi-daemon avahi-utils; systemctl restart avahi-daemon; nohup python3 /opt/echo.py server >/var/log/interop-echo.log 2>&1 &"',timeout=300)
    until(lambda:ssh('systemctl is-active avahi-daemon',timeout=8).strip()=='active',30,'QEMU Avahi readiness')
    # Local listener control before any negative policy observation.
    def listeners(name):
        rows=[json.loads(line) for line in guest(name,python_command(peer,'64','127.0.0.1','::1')).splitlines()]
        assert len(rows)==4 and all(row['reached'] for row in rows), str(rows)
        return True
    until(lambda:listeners('qemu'),30,'QEMU TCP/UDP listeners')
    suffix=uuid.uuid4().hex[:10]
    names=['interop-a-'+suffix]
    labels={'qemu':'qemu-'+suffix,names[0]:'ordinary-'+suffix}
    all_addresses={'qemu':until(lambda:addresses('qemu'),90,'QEMU dual-stack DHCP/RA')}
    for name,subnet,isolated in ((names[0],'192.168.105.0/24',False),):
        MACHINES.append(name)
        # Avahi is a workload-owned fixture, never part of guestd.
        setup='export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y -qq python3 iproute2 avahi-daemon avahi-utils dbus && mkdir -p /run/dbus && { test -S /run/dbus/system_bus_socket || dbus-daemon --system --fork; } && printf "[server]\\nhost-name='+name+'\\nallow-interfaces=eth0\\n[reflector]\\nenable-reflector=no\\n" >/etc/avahi/avahi-daemon.conf && { avahi-daemon --check || avahi-daemon --daemonize --no-drop-root; } && exec '+python_command(server,'server')
        args=[VMCTL,'run','-d','--name',name,'--shared-network',subnet]
        if isolated:args+=['--isolate-peers']
        progress('Start '+name+' '+subnet+(' isolated' if isolated else ' participating'))
        run(args+['ubuntu:24.04','sh','-c',setup],timeout=180)
        until(lambda:guest(name,'test -S /run/avahi-daemon/socket && echo ready'),600,name+' Avahi readiness')
        until(lambda:listeners(name),60,name+' TCP/UDP listeners')
        all_addresses[name]=until(lambda:addresses(name),90,name+' dual-stack addresses')
    import importlib.util
    spec=importlib.util.spec_from_file_location('services',ROOT/'scripts/fixtures/vmnet-ipv6-services.py')
    probe=importlib.util.module_from_spec(spec);spec.loader.exec_module(probe)
    for name,items in all_addresses.items():
        mac_control(name,items,probe)
        for family,address in items.items():
            scoped=address+'%eth0' if family=='ll6' else address
            rows=[json.loads(line) for line in guest(name,'EVIDENCE_SOURCE='+shlex.quote(address)+' '+python_command(peer,'64',scoped)).splitlines()]
            assert len(rows)==2 and all(row['reached'] for row in rows), f'{name} native-address source bind/listener control failed: {rows}'
    for target,expected in ((names[0],True),):
        matrix('qemu',target,all_addresses['qemu'],all_addresses[target],peer,expected)
        matrix(target,'qemu',all_addresses[target],all_addresses['qemu'],peer,expected)
    for name,label in labels.items():
        command='nohup avahi-publish-service '+shlex.quote(label)+' _clankerdgate._tcp 18081 > /tmp/interop-publish.log 2>&1 </dev/null &'
        if name=='qemu':command='sudo sh -c '+shlex.quote(command)
        guest(name,command)
    for name,label in labels.items():
        until(lambda: label in guest(name,'timeout 5 avahi-browse -rpt _clankerdgate._tcp || test \"$?\" = 124',timeout=10),30,name+' own Avahi publication')
    for target,expected in ((names[0],True),):
        mdns('qemu',target,labels[target],expected)
        mdns(target,'qemu',labels['qemu'],expected)
    # Mac's actual DNS-SD browser/resolver, random per-run labels avoid stale cache.
    process=start(['dns-sd','-B','_clankerdgate._tcp','local.'],'mac-dnssd.log')
    time.sleep(12);process.terminate();process.wait(timeout=5)
    browse=(WORK/'mac-dnssd.log').read_text()
    assert all(label in browse for label in labels.values()), 'Mac DNS-SD did not discover every healthy fixture'
    for name,label in labels.items():
        resolver=start(['dns-sd','-L',label,'_clankerdgate._tcp','local.'],f'mac-resolve-{name}.log')
        time.sleep(5);resolver.terminate();resolver.wait(timeout=5)
        resolved=(WORK/f'mac-resolve-{name}.log').read_text()
        assert '18081' in resolved and 'can be reached' in resolved, 'Mac DNS-SD resolution failed '+name
    progress('PASS Mac DNS-SD browse/resolve of QEMU and ordinary clankerd workloads')
    progress('PASS other-VMM bidirectional IPv4/IPv6 TCP/UDP and Avahi matrix')
    progress('COVERAGE: QEMU and clankerd share selected105. Cross-network isolation with another VMM, external IPv6 and bridged multicast are NOT tested.')


def cleanup():
    for name in MACHINES:
        try:run([VMCTL,'rm','-f',name],timeout=60,check=False)
        except Exception as error:progress('Cleanup machine: '+str(error))
    # Helper drops to the invoking UID; find only the process bound to our exact socket.
    try:
        listing=run(['ps','-axo','pid=,uid=,command='],check=False).stdout
        for line in listing.splitlines():
            fields=line.strip().split(None,2)
            if len(fields)==3 and fields[1]==str(os.getuid()) and str(WORK/'helper.sock') in fields[2] and 'vmnet-helper' in fields[2]:
                os.kill(int(fields[0]),signal.SIGTERM)
    except OSError:pass
    cleanup_failed=[]
    for process in reversed(PROCESSES):
        if process.poll() is None:
            try:process.terminate();process.wait(timeout=8)
            except (OSError,subprocess.TimeoutExpired):
                try:process.kill();process.wait(timeout=3)
                except (OSError,subprocess.TimeoutExpired):
                    cleanup_failed.append(process.pid)
    for output in LOGS:output.close()
    for name in ('overlay.qcow2','noble-server-cloudimg-arm64.img','seed.iso','key','key.pub'):
        (WORK/name).unlink(missing_ok=True)
    # Remove only owned VM disks, preserve logs/status for diagnosis.
    for path in (WORK/'state').rglob('*'):
        if path.is_file() and path.suffix in ('.img','.qcow2','.ext4'):path.unlink()
    progress('Retained evidence: '+str(WORK))
    if cleanup_failed:raise RuntimeError(f'Unable to reap owned process IDs {cleanup_failed}; see retained helper/QEMU logs')


if __name__=='__main__':
    try:main()
    finally:cleanup()
