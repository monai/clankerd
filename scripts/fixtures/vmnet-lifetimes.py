#!/usr/bin/env python3
"""Controlled real Ethernet fixture: short DHCP leases and fresh-prefix RA.

Only test traffic is changed; standard protocol timers run in bundled dhcpcd.
A fresh prefix avoids RFC4862's two-hour valid-lifetime protection.
"""
import errno
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import resource
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time

module = importlib.util.spec_from_file_location('family', Path(__file__).with_name('vmnet-family-filter.py'))
family = importlib.util.module_from_spec(module)
module.loader.exec_module(family)
PREFIX = 'fd73:736d:6f6b:7::'
DNS6 = 'fd73:736d:6f6b:7::53'


def checksum(data):
    data += bytes(len(data) % 2)
    total = sum(struct.unpack('!%dH' % (len(data) // 2), data))
    while total >> 16:
        total = (total & 65535) + (total >> 16)
    return (~total) & 65535


def ra():
    src = socket.inet_pton(socket.AF_INET6, 'fe80::1234')
    dst = socket.inet_pton(socket.AF_INET6, 'ff02::1')
    payload = struct.pack('!BBHBBHII', 134, 0, 0, 64, 0x80, 10, 0, 0)
    payload += struct.pack('!BBBBIII', 3, 4, 64, 0xc0, 16, 6, 0) + socket.inet_pton(socket.AF_INET6, PREFIX)
    payload += struct.pack('!BBHI', 25, 3, 0, 10) + socket.inet_pton(socket.AF_INET6, DNS6)
    domain = b'\x0bra-lifetime\x04test\x00'
    payload += struct.pack('!BBHI', 31, 4, 0, 10) + domain + bytes(24 - len(domain))
    payload += bytes([1, 1]) + bytes.fromhex('020000001234')
    pseudo = src + dst + struct.pack('!I3xB', len(payload), 58)
    value = checksum(pseudo + payload)
    payload = payload[:2] + struct.pack('!H', value) + payload[4:]
    return bytes.fromhex('33330000000102000000123486dd') + struct.pack('!IHBB', 6 << 28, len(payload), 58, 255) + src + dst + payload


def dhcp(frame):
    if len(frame) < 14 + 20 + 8 + 240 or frame[12:14] != b'\x08\x00':
        return None
    ihl = (frame[14] & 15) * 4
    udp = 14 + ihl
    if frame[23] != 17 or frame[udp:udp+4] not in (b'\x00C\x00D', b'\x00D\x00C'):
        return None
    bootp = udp + 8
    if frame[bootp+236:bootp+240] != b'\x63\x82\x53\x63':
        return None
    options = {}
    pos = bootp + 240
    while pos < len(frame):
        code = frame[pos]
        pos += 1
        if code == 255:
            break
        if code == 0:
            continue
        if pos >= len(frame):
            return None
        length = frame[pos]
        pos += 1
        options[code] = frame[pos:pos+length]
        pos += length
    return ihl, udp, bootp, options


def reply(frame, parsed, ack_count, nak=False):
    ihl, udp, bootp, options = parsed
    if options.get(53) not in (b'\x02', b'\x05'):
        return frame
    options.update({51: struct.pack('!I', 24), 58: struct.pack('!I', 8), 59: struct.pack('!I', 16)})
    options[6] = socket.inet_aton('192.0.2.53' if ack_count < 2 else '192.0.2.54')
    options[15] = b'lifetime.test'
    options[119] = b'\x08lifetime\x04test\x00'
    if nak:
        options = {53: b'\x06', 54: options[54]}
    payload = bytearray(frame[bootp:bootp+240])
    if nak:
        payload[16:20] = bytes(4)
    payload += b''.join(bytes([key, len(value)]) + value for key, value in options.items()) + b'\xff'
    header = bytearray(frame[:bootp])
    struct.pack_into('!H', header, 16, ihl + 8 + len(payload))
    struct.pack_into('!H', header, udp + 4, 8 + len(payload))
    struct.pack_into('!H', header, udp + 6, 0)  # UDPv4 checksum optional.
    struct.pack_into('!H', header, 24, 0)
    struct.pack_into('!H', header, 24, checksum(bytes(header[14:14+ihl])))
    return bytes(header) + payload


def dhcp6_reply(frame, count):
    if len(frame) < 66 or frame[12:14] != b'\x86\xdd' or frame[20] != 17 or frame[54:58] != struct.pack('!HH', 546, 547):
        return None
    request = frame[62:]
    if request[0] not in (1, 3, 5, 6):
        return None
    options = {}
    pos = 4
    while pos + 4 <= len(request):
        code, length = struct.unpack_from('!HH', request, pos)
        pos += 4
        if pos + length > len(request):
            return None
        options[code] = request[pos:pos+length]
        pos += length
    if 1 not in options or 3 not in options or len(options[3]) < 12:
        return None
    def option(code, value):
        return struct.pack('!HH', code, len(value)) + value
    iaaddr = socket.inet_pton(socket.AF_INET6, 'fd73:736d:6f6b:8::99') + struct.pack('!II', 12, 24)
    iana = options[3][:4] + struct.pack('!II', 8, 16) + option(5, iaaddr)
    payload = bytes([2 if request[0] == 1 else 7]) + request[1:4]
    payload += option(1, options[1]) + option(2, bytes.fromhex('00030001020000001234'))
    payload += option(3, iana) + option(23, socket.inet_pton(socket.AF_INET6, '2001:db8::53' if count < 2 else '2001:db8::54'))
    src = socket.inet_pton(socket.AF_INET6, 'fe80::1234')
    dst = frame[22:38]
    udp = struct.pack('!HHHH', 547, 546, len(payload) + 8, 0) + payload
    value = checksum(src + dst + struct.pack('!I3xB', len(udp), 17) + udp) or 65535
    udp = udp[:6] + struct.pack('!H', value) + udp[8:]
    return frame[6:12] + bytes.fromhex('02000000123486dd') + struct.pack('!IHBB', 6 << 28, len(udp), 17, 64) + src + dst + udp


class LifetimeProxy(family.Proxy):
    def __init__(self, directory, upstream, controls):
        super().__init__(directory, upstream, 'dual')
        self.controls = Path(controls)
        self.next_ra = 0
        self.acks = 0
        self.replies6 = 0
        self.nak_sent = False
        self.rebinds = 0

    def event(self, name):
        with (self.controls / 'events').open('a') as stream:
            stream.write(name + '\n')

    def pump(self, timeout=0.1):
        for key, _ in self.selector.select(timeout):
            frame, peer = key.fileobj.recvfrom(65536)
            outbound = key.fileobj is self.front
            if outbound:
                if self.peer is None:
                    self.peer = peer
                    self.front.connect(peer)
                target = self.back
            else:
                target = self.front
                if self.peer is None:
                    continue
            if outbound:
                response6 = dhcp6_reply(frame, self.replies6 + 1)
                if response6 is not None:
                    message = frame[62]
                    self.event('renew6' if message == 5 else 'rebind6' if message == 6 else 'request6')
                    if not (self.controls / 'drop-dhcp6').exists():
                        if message != 1:
                            self.replies6 += 1
                            self.event('reply6')
                        self.front.send(response6)
                    continue
            # Suppress real RAs/DHCPv6 so only the controlled prefix/DNS exists.
            if len(frame) >= 14 and frame[12:14] == b'\x86\xdd':
                if not outbound:
                    continue
                if len(frame) > 54 and frame[20] == 58 and frame[54] in (133, 134):
                    continue
            parsed = dhcp(frame)
            if parsed:
                options = parsed[3]
                if outbound and options.get(53) == b'\x03':
                    bootp = parsed[2]
                    if frame[bootp+12:bootp+16] != bytes(4):
                        self.event('rebind' if frame[30:34] == bytes([255])*4 else 'renew')
                if not outbound and options.get(53) in (b'\x02', b'\x05'):
                    if (self.controls / 'drop-dhcp').exists():
                        continue
                    nak = options.get(53) == b'\x05' and (self.controls / 'nak').exists() and not self.nak_sent
                    if nak:
                        self.nak_sent = True
                        self.event('nak')
                    elif options.get(53) == b'\x05':
                        self.acks += 1
                        self.event('ack')
                    frame = reply(frame, parsed, self.acks, nak)
            target.send(frame)
        if self.peer is not None and time.monotonic() >= self.next_ra and not (self.controls / 'stop-ra').exists():
            self.front.send(ra())
            self.next_ra = time.monotonic() + 2


def self_test():
    with tempfile.TemporaryDirectory() as directory:
        helper = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        helper.bind(directory + '/real.net')
        helper.settimeout(1)
        guest = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        guest.bind(directory + '/guest.net')
        guest.settimeout(1)
        proxy = LifetimeProxy(directory, directory + '/real.net', directory)
        guest.connect(proxy.path)
        guest.send(b'VFKT')
        proxy.pump()
        _, peer = helper.recvfrom(4096)
        helper.connect(peer)
        # Drain startup RA so the next response is the DHCPv4 wire packet.
        guest.recv(4096)
        bootp = bytearray(240)
        bootp[0] = 2
        bootp[236:240] = bytes.fromhex('63825363')
        options = bytes.fromhex('3501053604c0a86901') + bytes([119, 11]) + b'\x04home\x04arpa\x00' + b'\xff'
        body = bootp + options
        ip = struct.pack('!BBHHHBBH4s4s', 0x45, 0, 28 + len(body), 0, 0, 64, 17, 0,
                         socket.inet_aton('192.168.105.1'), socket.inet_aton('255.255.255.255'))
        helper.send(bytes(12) + b'\x08\x00' + ip + struct.pack('!HHHH', 67, 68, 8+len(body), 0) + body)
        proxy.pump()
        output = guest.recv(4096)
        # Decode option TLVs independently at the actual guest Ethernet boundary.
        options = {}
        cursor = 14 + (output[14] & 15)*4 + 8 + 240
        while cursor < len(output):
            code = output[cursor]; cursor += 1
            if code == 255: break
            if code == 0: continue
            length = output[cursor]; cursor += 1
            options[code] = output[cursor:cursor+length]; cursor += length
        expected = b'\x08lifetime\x04test\x00'
        assert options[119] == expected, f'guest DHCP search payload={options[119]!r}; expected lifetime.test'
        print('PASS actual fixture Unixgram DHCP reply supplies lifetime.test in option119')
        proxy.close(); helper.close(); guest.close()

def main():
    args = sys.argv[1:]
    if args == ['--self-test']:
        self_test()
        return 0
    original = Path(args[args.index('--spec') + 1])
    spec = json.loads(original.read_text())
    binary = os.environ['VMNET_TEST_REAL_VMSPAWN']
    os.environ['DYLD_FALLBACK_LIBRARY_PATH'] = os.environ.get('VMNET_TEST_LIBRARY_PATH', '/opt/homebrew/lib:/usr/local/lib:/usr/lib')
    if not spec.get('net'):
        os.execv(binary, [binary, *args])
    with tempfile.TemporaryDirectory(prefix='vmnet-lifetime-', dir='/tmp') as directory:
        proxy = LifetimeProxy(directory, spec['net']['socket'], os.environ['VMNET_LIFETIME_CONTROL'])
        spec['net']['socket'] = proxy.path
        modified = Path(directory) / 'spec.json'
        modified.write_text(json.dumps(spec))
        args[args.index('--spec') + 1] = str(modified)
        child = subprocess.Popen([binary, *args], pass_fds=(3,))
        Path(os.environ['VMNET_LIFETIME_CONTROL'], 'native.pid').write_text(str(child.pid))
        limits = resource.getrlimit(resource.RLIMIT_NOFILE)
        Path(os.environ['VMNET_LIFETIME_CONTROL'], 'native-limits').write_text(f'inherited native RLIMIT_NOFILE soft={limits[0]} hard={limits[1]}\n')
        print(f'lifetime fixture {time.time():.3f}: wrapper pid={os.getpid()} native pid={child.pid}', flush=True)
        transport_error = None
        def stop(signum, _):
            print(f'lifetime fixture {time.time():.3f}: forwarding signal {signum}; native status={child.poll()}', flush=True)
            if child.poll() is None:
                child.send_signal(signum)
        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)
        try:
            while child.poll() is None:
                try:
                    proxy.pump()
                except OSError as error:
                    if error.errno not in {errno.ECONNRESET, errno.ECONNREFUSED, errno.ENOTCONN, errno.EPIPE, errno.ENOENT}:
                        raise
                    if transport_error != error.errno:
                        print(f'lifetime fixture {time.time():.3f}: network transport error {error}; native status={child.poll()}', flush=True)
                        transport_error = error.errno
                    # Teardown or backend loss must not terminate a live guest.
                    time.sleep(0.05)
            print(f'lifetime fixture {time.time():.3f}: native pid={child.pid} exited status={child.returncode}', flush=True)
            return child.returncode
        finally:
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=10)
            proxy.close()


if __name__ == '__main__':
    sys.exit(main())
