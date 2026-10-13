#!/usr/bin/env python3
"""Real fixture server survives abandonment and fragmented stream messages."""
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time

fixture = Path(__file__).with_name('vmnet-recovery-services.py')
with tempfile.TemporaryDirectory(prefix='vmnet-unix-test-') as directory:
    endpoint = str(Path(directory) / 'host.sock')
    server = subprocess.Popen([sys.executable, str(fixture), 'unix-server', endpoint], stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 3
        while not Path(endpoint).exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        assert Path(endpoint).exists(), 'Unix fixture did not bind'
        os.kill(server.pid, signal.SIGSTOP)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as abandoned:
            abandoned.connect(endpoint)
            abandoned.sendall(b'abandoned-request')
            abandoned.shutdown(socket.SHUT_RDWR)
        os.kill(server.pid, signal.SIGCONT)
        time.sleep(0.1)
        assert server.poll() is None, 'one abandoned client terminated the Unix fixture'
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(2)
            client.connect(endpoint)
            expected = b'recovery-unix'
            received = bytearray()
            for fragment in (b're', b'cov', b'ery-', b'unix'):
                client.sendall(fragment)
                # Observe each prefix before sending the remainder: a stream
                # peer must survive fragmentation instead of closing after recv.
                target = len(received) + len(fragment)
                while len(received) < target:
                    data = client.recv(target - len(received))
                    assert data, 'Unix echo closed before a fragmented request completed'
                    received += data
            assert received == expected
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(2)
            client.connect(endpoint)
            client.sendall(b'next-client')
            assert client.recv(4096) == b'next-client'
        print('PASS real Unix fixture survives abandoned clients and fragmented requests')
    finally:
        if server.poll() is None:
            os.kill(server.pid, signal.SIGCONT)
            server.terminate()
        server.wait()
        error = server.stderr.read().decode()
        if error:
            print(error, file=sys.stderr)
