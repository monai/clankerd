#!/usr/bin/env python3
"""Real guest listeners and Mac host packet probes for the IPv6 gate."""
import ipaddress
import json
import socket
import sys
import threading
import time


def server():
    def serve(family, kind, port):
        sock = socket.socket(family, kind)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        if family == socket.AF_INET6:
            sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        sock.bind(('::' if family == socket.AF_INET6 else '0.0.0.0', port))
        if kind == socket.SOCK_STREAM:
            sock.listen()
            while True:
                conn, _ = sock.accept()
                with conn:
                    conn.sendall(conn.recv(2048))
        else:
            while True:
                data, peer = sock.recvfrom(2048)
                sock.sendto(data, peer)
    for family in (socket.AF_INET, socket.AF_INET6):
        for kind, port in ((socket.SOCK_STREAM, 18081), (socket.SOCK_DGRAM, 18082)):
            threading.Thread(target=serve, args=(family, kind, port), daemon=True).start()
    while True:
        time.sleep(60)


def external(address, port):
    # Literal IPv6 and AF_INET6 prohibit IPv4 fallback.
    assert ipaddress.ip_address(address).version == 6
    with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as sock:
        sock.settimeout(15)
        sock.connect((address, int(port), 0, 0))
    print(f'PASS IPv6 external TCP {address}:{port}')


def probe(address, scope=0):
    family = socket.AF_INET6 if ':' in address else socket.AF_INET
    for kind, port in ((socket.SOCK_STREAM, 18081), (socket.SOCK_DGRAM, 18082)):
        with socket.socket(family, kind) as sock:
            sock.settimeout(3)
            endpoint = (address, port, 0, scope) if family == socket.AF_INET6 else (address, port)
            sock.connect(endpoint)
            sock.send(b'vmnet-family-smoke')
            assert sock.recv(2048) == b'vmnet-family-smoke'
    print(f'PASS local TCP/UDP {address}' + (f'%{socket.if_indextoname(scope)}' if scope else ''))


def host(status_path, expected):
    machine = json.load(open(status_path))[0]
    network = machine['network']
    print(json.dumps(network, indent=2))
    assert network['ready'], network
    details = [d for d in network['address_details'] if not d['tentative'] and not d['dad_failed']]
    v4 = [d['address'] for d in details if ipaddress.ip_address(d['address']).version == 4]
    v6 = [d['address'] for d in details if ipaddress.ip_address(d['address']).version == 6]
    if expected == 'ipv4-services':
        assert v4 and network['ipv4']['local_available'], network
        probe(v4[0])
        print('PASS IPv4 TCP/UDP services with current network configuration')
        return
    if expected == 'ipv6':
        assert not v4, f'IPv4 present before IPv6-only workload startup: {v4}'
    if expected == 'ipv4':
        assert not any(not ipaddress.ip_address(a).is_link_local for a in v6), v6
        assert not network['ipv6']['default_route'], network
        assert not any(':' in r['destination'] and r['prefix_len'] == 0 for r in network['routes'])
    if expected != 'ipv6':
        assert v4 and network['ipv4']['local_available'], network
        probe(v4[0])
    if expected != 'ipv4':
        assert v6 and network['ipv6']['local_available'], network
        global6 = [a for a in v6 if not ipaddress.ip_address(a).is_link_local]
        for address in global6:
            probe(address)
        local6 = [a for a in v6 if ipaddress.ip_address(a).is_link_local]
        assert local6, 'No usable link-local IPv6 address'
        # Guest eth0's zone has no meaning on the Mac. Try actual Mac interfaces;
        # success identifies the vmnet host attachment even with no guest IPv4.
        found = False
        for index, name in sorted(socket.if_nameindex(), key=lambda item: not item[1].startswith("bridge")):
            if name == 'lo0':
                continue
            try:
                probe(local6[0], index)
                found = True
                break
            except (OSError, AssertionError):
                pass
        assert found, 'Scoped IPv6 TCP/UDP failed on every Mac interface'
    print(f'PASS {expected} startup and family-neutral inspection')


if __name__ == '__main__':
    action = sys.argv[1]
    if action == 'server':
        server()
    elif action == 'external':
        external(*sys.argv[2:])
    elif action == 'host':
        host(*sys.argv[2:])
