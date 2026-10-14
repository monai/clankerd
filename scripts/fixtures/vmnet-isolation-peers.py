#!/usr/bin/env python3
"""Observe actual normal-hop peer delivery without assuming subnet isolation."""
import json
import os
import socket
import sys


def observe(address, hops):
    bare, _, zone = address.partition('%')
    family = socket.AF_INET6 if ':' in bare else socket.AF_INET
    scope = socket.if_nametoindex(zone) if zone else 0
    rows = []
    base = int(os.environ.get("EVIDENCE_PORT_BASE", "18081"))
    for kind, port in ((socket.SOCK_STREAM, base), (socket.SOCK_DGRAM, base + 1)):
        row = {'address': address, 'protocol': 'TCP' if kind == socket.SOCK_STREAM else 'UDP', 'hops': hops, 'reached': False}
        with socket.socket(family, kind) as connection:
            connection.settimeout(2)
            connection.setsockopt(socket.IPPROTO_IPV6 if family == socket.AF_INET6 else socket.IPPROTO_IP,
                                  socket.IPV6_UNICAST_HOPS if family == socket.AF_INET6 else socket.IP_TTL, hops)
            endpoint = (bare, port, 0, scope) if family == socket.AF_INET6 else (bare, port)
            try:
                connection.connect(endpoint)
                connection.sendall(b'isolation-evidence')
                row['reached'] = connection.recv(2048) == b'isolation-evidence'
            except OSError as error:
                row['error'] = str(error)
        rows.append(row)
    return rows


if __name__ == '__main__':
    for address in sys.argv[2:]:
        for row in observe(address, int(sys.argv[1])):
            print(json.dumps(row), flush=True)
