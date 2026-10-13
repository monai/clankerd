#!/usr/bin/env python3
"""Exercise the CLI against real dual listeners and inspected configuration."""
import json
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

fixture = Path(__file__).with_name('vmnet-ipv6-services.py')
with tempfile.TemporaryDirectory(prefix='vmnet-service-test-') as directory:
    snapshot = Path(directory) / 'dual.json'
    network = {
        'ready': True,
        'address_details': [
            {'address': address, 'tentative': False, 'dad_failed': False}
            for address in ('127.0.0.1', '::1')
        ],
        'ipv4': {'local_available': True},
        'ipv6': {'local_available': True, 'default_route': True},
        'routes': [{'destination': '::', 'prefix_len': 0}],
    }
    snapshot.write_text(json.dumps([{'network': network}]))
    child = subprocess.Popen([sys.executable, str(fixture), 'server'])
    try:
        for attempt in range(50):
            try:
                with socket.create_connection(('::1', 18081), timeout=0.1):
                    break
            except OSError:
                time.sleep(0.02)
        else:
            raise AssertionError('Real dual listeners did not start')
        def host(mode):
            return subprocess.run([sys.executable, str(fixture), 'host', str(snapshot), mode],
                                  capture_output=True, text=True)
        services = host('ipv4-services')
        assert services.returncode == 0, services.stdout + services.stderr
        assert 'PASS local TCP/UDP 127.0.0.1' in services.stdout
        strict = host('ipv4')
        assert strict.returncode != 0 and "AssertionError: ['::1']" in strict.stderr, strict.stdout + strict.stderr
        network['address_details'] = network['address_details'][:1]
        network['ipv6']['default_route'] = False
        network['routes'] = []
        snapshot.write_text(json.dumps([{'network': network}]))
        ipv4_only = host('ipv4')
        assert ipv4_only.returncode == 0, ipv4_only.stdout + ipv4_only.stderr
        print('PASS real IPv4 service probes accept dual stack; IPv4-only startup still rejects IPv6')
    finally:
        child.terminate()
        child.wait()
