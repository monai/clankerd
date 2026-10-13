#!/usr/bin/env python3
"""Mac-to-guest TCP/UDP checks on an explicitly selected physical LAN."""
import importlib.util
import ipaddress
import json
from pathlib import Path
import socket
import sys

spec = importlib.util.spec_from_file_location('services', Path(__file__).with_name('vmnet-ipv6-services.py'))
services = importlib.util.module_from_spec(spec)
spec.loader.exec_module(services)

machine = json.load(open(sys.argv[1]))[0]
interface, family = sys.argv[2:]
assert machine['host_config']['network_mode'] == 'bridged'
assert machine['host_config']['bridge_interface'] == interface
network = machine['network']
print(json.dumps(network, indent=2))
assert network['ready'], network
addresses = [d['address'] for d in network['address_details'] if not d['tentative'] and not d['dad_failed']]
assert addresses, 'No usable assigned addresses'
v4 = [a for a in addresses if ipaddress.ip_address(a).version == 4]
v6 = [a for a in addresses if ipaddress.ip_address(a).version == 6]
if family == 'ipv6':
    assert not v4, f'IPv4 present in preboot IPv6-only LAN fixture: {v4}'
    assert v6 and network['ipv6']['local_available']
elif family == 'ipv4':
    assert v4 and network['ipv4']['local_available']
    assert not network['ipv6']['default_route']
    assert not any(not ipaddress.ip_address(a).is_link_local for a in v6)
for address in v4:
    services.probe(address)
if family != 'ipv4':
    zone = socket.if_nametoindex(interface)
    for address in v6:
        services.probe(address, zone if ipaddress.ip_address(address).is_link_local else 0)
print(f'PASS Mac LAN TCP/UDP via explicitly selected {interface}, {family} fixture')
