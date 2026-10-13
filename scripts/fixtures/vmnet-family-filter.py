#!/usr/bin/env python3
"""Unprivileged preboot Ethernet filter; the installed vmnet-helper stays real."""
import json
import os
from pathlib import Path
import selectors
import signal
import socket
import struct
import subprocess
import sys
import tempfile


def allowed(frame, family):
    # libkrun/vfkit's short handshake must reach helper before its peer connect.
    if len(frame) < 14:
        return True
    offset = 12
    kind = struct.unpack_from('!H', frame, offset)[0]
    while kind in (0x8100, 0x88A8):
        offset += 4
        if len(frame) < offset + 2:
            return False
        kind = struct.unpack_from('!H', frame, offset)[0]
    return kind not in ({0x0800, 0x0806} if family == 'ipv6' else {0x86DD})


class Proxy:
    def __init__(self, directory, upstream, family):
        self.family = family
        self.front = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        self.back = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        self.path = str(Path(directory) / 'vm.net')
        self.front.bind(self.path)
        self.back.bind(str(Path(directory) / 'helper.net'))
        self.back.connect(upstream)
        self.peer = None
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.front, selectors.EVENT_READ)
        self.selector.register(self.back, selectors.EVENT_READ)
        self.dropped = 0

    def pump(self, timeout=0.1):
        for key, _ in self.selector.select(timeout):
            frame, peer = key.fileobj.recvfrom(65536)
            if key.fileobj is self.front:
                if self.peer is None:
                    if not peer:
                        raise RuntimeError('VMM Unixgram client has no return address')
                    self.peer = peer
                    self.front.connect(peer)
                target = self.back
            else:
                target = self.front
                if self.peer is None:
                    continue
            if allowed(frame, self.family):
                target.send(frame)
            else:
                self.dropped += 1

    def close(self):
        self.selector.close()
        self.front.close()
        self.back.close()


def main():
    args = sys.argv[1:]
    if '--self-test' in args:
        self_test()
        return 0
    original = Path(args[args.index('--spec') + 1])
    spec = json.loads(original.read_text())
    family = os.environ.get('VMNET_TEST_FAMILY', 'dual')
    binary = os.environ['VMNET_TEST_REAL_VMSPAWN']
    if family == 'dual':
        os.execv(binary, [binary, *args])
    with tempfile.TemporaryDirectory(prefix='vmnet-filter-', dir='/tmp') as directory:
        proxy = Proxy(directory, spec['net']['socket'], family)
        spec['net']['socket'] = proxy.path
        modified = Path(directory) / 'spec.json'
        modified.write_text(json.dumps(spec))
        args[args.index('--spec') + 1] = str(modified)
        child = subprocess.Popen([binary, *args], pass_fds=(3,))
        def stop(signum, _):
            if child.poll() is None:
                child.send_signal(signum)
        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)
        try:
            while child.poll() is None:
                proxy.pump()
            print(f'vmnet {family} fixture dropped {proxy.dropped} frames', flush=True)
            return child.returncode
        finally:
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=10)
            proxy.close()


def self_test():
    def frame(kind):
        return bytes(12) + struct.pack('!H', kind) + bytes(64)
    assert not allowed(frame(0x0800), 'ipv6')
    assert not allowed(frame(0x0806), 'ipv6')
    assert allowed(frame(0x86DD), 'ipv6')
    assert not allowed(frame(0x86DD), 'ipv4')
    assert allowed(frame(0x0800), 'ipv4')
    assert allowed(b'VFKT', 'ipv6')
    vlan = bytes(12) + struct.pack('!HHH', 0x8100, 1, 0x0800) + bytes(64)
    assert not allowed(vlan, 'ipv6')
    with tempfile.TemporaryDirectory(prefix='vmnet-filter-test-') as directory:
        helper = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        helper.bind(directory + '/real.net')
        helper.settimeout(0.2)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        client.bind(directory + '/client.net')
        client.settimeout(0.2)
        proxy = Proxy(directory, directory + '/real.net', 'ipv6')
        client.connect(proxy.path)
        client.send(frame(0x0800))
        proxy.pump()
        try:
            helper.recv(2048)
            raise AssertionError('IPv4 escaped preboot filter')
        except socket.timeout:
            pass
        packet = frame(0x86DD)
        client.send(packet)
        proxy.pump()
        got, peer = helper.recvfrom(2048)
        assert got == packet
        helper.connect(peer)  # vmnet-helper uses this exact peer handshake.
        helper.send(packet)
        proxy.pump()
        assert client.recv(2048) == packet
        helper.send(frame(0x0806))
        proxy.pump()
        try:
            client.recv(2048)
            raise AssertionError('ARP escaped return filter')
        except socket.timeout:
            pass
        assert proxy.dropped == 2
        proxy.close()
        client.close()
        helper.close()
    print('PASS local fixture: VLAN family filtering and real Unixgram peer handshake')


if __name__ == '__main__':
    sys.exit(main())
