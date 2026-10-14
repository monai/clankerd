#!/usr/bin/env python3
"""Observe actual normal-hop peer delivery without assuming subnet isolation."""
import json
import time
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
        row = {'family': 'IPv6-link-local' if ':' in bare and bare.lower().startswith('fe80:') else ('IPv6-global' if ':' in bare else 'IPv4'), 'address': address, 'protocol': 'TCP' if kind == socket.SOCK_STREAM else 'UDP', 'hops': hops, 'reached': False}
        for attempt in range(2):
            with socket.socket(family, kind) as connection:
                connection.settimeout(3 if attempt == 0 else 2)
                connection.setsockopt(socket.IPPROTO_IPV6 if family == socket.AF_INET6 else socket.IPPROTO_IP,
                                      socket.IPV6_UNICAST_HOPS if family == socket.AF_INET6 else socket.IP_TTL, hops)
                endpoint = (bare, port, 0, scope) if family == socket.AF_INET6 else (bare, port)
                try:
                    connection.connect(endpoint)
                    connection.sendall(b'isolation-evidence')
                    reply=bytearray()
                    while len(reply)<len(b'isolation-evidence'):
                        data=connection.recv(2048)
                        if not data:break
                        reply+=data
                        if kind==socket.SOCK_DGRAM:break
                    row['reached'] = reply == b'isolation-evidence'
                except OSError as error:
                    row['error'] = str(error)
            if row['reached']:break
            if attempt == 0:time.sleep(0.2)
        rows.append(row)
    return rows


def summary(path):
    groups={}
    for line in open(path):
        row=json.loads(line)
        key=(row.get('source','guest'),row.get('target','Mac'),row['hops'])
        groups.setdefault(key,[]).append(f"{row['family']}/{row['protocol']}={'reached' if row['reached'] else 'failed'}")
    for (source,target,hops),results in groups.items():
        print(f"{source}->{target} hop{hops}: "+', '.join(results))


if __name__ == '__main__':
    if sys.argv[1]=='summary':
        summary(sys.argv[2]);sys.exit(0)
    for address in sys.argv[2:]:
        for row in observe(address, int(sys.argv[1])):
            print(json.dumps(row), flush=True)
