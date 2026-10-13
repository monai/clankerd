#!/usr/bin/env python3
"""Real VM peer probes. A one-hop limit distinguishes one Ethernet network
from connectivity routed through the Mac between separately selected networks.
"""
import ipaddress
import json
import socket
import sys


def targets(path):
    network = json.load(open(path))[0]['network']
    addresses = [a['address'] for a in network['address_details'] if not a['tentative'] and not a['dad_failed']]
    ipv4 = [a for a in addresses if ipaddress.ip_address(a).version == 4]
    ipv6 = [a for a in addresses if ipaddress.ip_address(a).version == 6]
    assert ipv4 and ipv6, network
    ipv6.sort(key=lambda a: ipaddress.ip_address(a).is_link_local)
    v6 = ipv6[0]
    if ipaddress.ip_address(v6).is_link_local:
        v6 += '%eth0'
    linklocal = next(a for a in ipv6 if ipaddress.ip_address(a).is_link_local) + '%eth0'
    print(ipv4[0], v6, linklocal)


def peer(address4, address6, expected):
    allowed = expected == 'allowed'
    for address in (address4, address6):
        family = socket.AF_INET6 if ':' in address else socket.AF_INET
        bare, _, zone = address.partition('%')
        scope = socket.if_nametoindex(zone) if zone else 0
        for kind, port in ((socket.SOCK_STREAM, 18081), (socket.SOCK_DGRAM, 18082)):
            connected = False
            error = None
            with socket.socket(family, kind) as sock:
                sock.settimeout(2)
                if family == socket.AF_INET6:
                    sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_UNICAST_HOPS, 1)
                    endpoint = (bare, port, 0, scope)
                else:
                    sock.setsockopt(socket.IPPROTO_IP, socket.IP_TTL, 1)
                    endpoint = (bare, port)
                try:
                    sock.connect(endpoint)
                    sock.send(b'vmnet-shared-peer')
                    connected = sock.recv(2048) == b'vmnet-shared-peer'
                except OSError as exc:
                    error = str(exc)
            assert connected == allowed, (address, kind, allowed, connected, error)
            print(f'PASS {expected} peer {address}:{port}' + (f' ({error})' if error else ''))


if __name__ == '__main__':
    if sys.argv[1] == 'targets':
        targets(sys.argv[2])
    elif sys.argv[1] == 'peer':
        peer(*sys.argv[2:])
