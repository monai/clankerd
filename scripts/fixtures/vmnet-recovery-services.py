#!/usr/bin/env python3
"""Real Unix binding and upgraded workload output during Ethernet loss."""
import json
import os
from pathlib import Path
import signal
import socket
import struct
import sys
import threading
import time


def rpc(connection, method, parameters, upgrade=False):
    connection.sendall(json.dumps({'method': method, 'parameters': parameters, 'upgrade': upgrade}).encode() + b'\0')
    reply = bytearray()
    while True:
        byte = connection.recv(1)
        if byte == b'\0':
            break
        assert byte, 'management closed before reply'
        reply += byte
    response = json.loads(reply)
    assert 'error' not in response, response
    return response['parameters']


def exact(connection, count):
    result = bytearray()
    while len(result) < count:
        data = connection.recv(count-len(result))
        assert data, 'workload output stream closed'
        result += data
    return result


if __name__ == '__main__':
    action = sys.argv[1]
    if action == 'recovery-server':
        Path('/tmp/recovery-workload.pid').write_text(str(os.getpid()))
        signal.signal(signal.SIGUSR1, lambda *_: Path('/tmp/recovery-signal').write_text(str(time.time())))
        def output():
            while True:
                print('recovery-heartbeat', time.time(), flush=True)
                time.sleep(0.2)
        threading.Thread(target=output, daemon=True).start()
        server()  # Ethernet listeners from vmnet-ipv6-services.py prepended by the gate.
    elif action == 'ready':
        pid = Path('/tmp/recovery-workload.pid').read_text().strip()
        assert pid.isdigit() and Path('/proc', pid).exists(), 'workload is not running'
        # Readiness of fixture listeners is independent of Ethernet addresses.
        for kind, port in ((socket.SOCK_STREAM, 18081), (socket.SOCK_DGRAM, 18082)):
            with socket.socket(socket.AF_INET, kind) as connection:
                connection.settimeout(0.2)
                connection.connect(('127.0.0.1', port))
                connection.sendall(b'recovery-ready')
                assert connection.recv(4096) == b'recovery-ready'
        print(pid)
    elif action == 'unix-server':
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(sys.argv[2])
        listener.listen()
        while True:
            connection, _ = listener.accept()
            with connection:
                connection.sendall(connection.recv(4096))
    elif action == 'bind':
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(10)
            connection.connect(sys.argv[2])
            rpc(connection, 'io.clankerd.Forwarding.Listen', {
                'listen': {'kind': 'unix', 'path': '/tmp/recovery-binding.sock'},
                'target': {'kind': 'unix', 'path': sys.argv[3]},
            })
        print('PASS host Unix binding established')
    elif action == 'attach':
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(5)
            connection.connect(sys.argv[2])
            rpc(connection, 'io.clankerd.Guest.Attach', {}, upgrade=True)
            output = bytearray()
            started = time.time()
            current = False
            while not current:
                channel, count = struct.unpack('!BI', exact(connection, 5))
                data = exact(connection, count)
                assert channel != 3, data
                if channel == 1:
                    output += data
                    for line in output.decode(errors='replace').splitlines():
                        words = line.split()
                        if len(words) == 2 and words[0] == 'recovery-heartbeat':
                            try:
                                current |= float(words[1]) >= started
                            except ValueError:
                                pass
        print('PASS workload attach/output through vsock')
    elif action == 'guest-unix':
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(5)
            connection.connect('/tmp/recovery-binding.sock')
            connection.sendall(b'recovery-unix')
            assert connection.recv(4096) == b'recovery-unix'
        print('PASS host Unix binding while Ethernet is unavailable')
    elif action == 'kill-client':
        clients = [int(entry.name) for entry in Path('/proc').iterdir()
                   if entry.name.isdigit() and (entry / 'comm').exists()
                   and (entry / 'comm').read_text().strip() == 'dhcpcd']
        assert len(clients) == 1, clients
        os.kill(clients[0], signal.SIGKILL)
        print('Killed configuration client', clients[0])
