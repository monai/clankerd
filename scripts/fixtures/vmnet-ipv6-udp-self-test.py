#!/usr/bin/env python3
"""Exercise destination-preserving IPv6 UDP replies on real loopback sockets."""
import importlib.util
from pathlib import Path
import socket
import struct

spec = importlib.util.spec_from_file_location('services', Path(__file__).with_name('vmnet-ipv6-services.py'))
services = importlib.util.module_from_spec(spec)
spec.loader.exec_module(services)

with socket.socket(socket.AF_INET6, socket.SOCK_DGRAM) as server, socket.socket(socket.AF_INET6, socket.SOCK_DGRAM) as client:
    server.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
    server.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_RECVPKTINFO, 1)
    server.bind(('::', 0))
    server.settimeout(2)
    client.settimeout(2)
    client.connect(('::1', server.getsockname()[1]))
    client.send(b'ipv6-source')

    class ObservedSocket:
        def recvmsg(self, *args):
            return server.recvmsg(*args)

        def sendmsg(self, data, controls, flags, peer):
            assert len(controls) == 1
            level, kind, value = controls[0]
            assert (level, kind) == (socket.IPPROTO_IPV6, socket.IPV6_PKTINFO)
            address, interface = struct.unpack('=16sI', value)
            assert socket.inet_ntop(socket.AF_INET6, address) == '::1'
            assert interface in [index for index, name in socket.if_nameindex() if name in ('lo', 'lo0')]
            return server.sendmsg(data, controls, flags, peer)

    services.echo_ipv6_datagram(ObservedSocket())
    assert client.recv(2048) == b'ipv6-source'
print('PASS real IPv6 UDP reply preserves received destination and interface')
