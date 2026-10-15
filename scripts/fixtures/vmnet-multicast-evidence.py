import json
import os
import socket
import struct
import threading
import time


def multicast_listen():
    def receive(family):
        sock = socket.socket(family, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        if family == socket.AF_INET:
            sock.bind(('0.0.0.0', 5353))
            sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP,
                            socket.inet_aton('224.0.0.251') + socket.inet_aton('0.0.0.0') + struct.pack('@I', socket.if_nametoindex('eth0')))
        else:
            sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
            sock.bind(('::', 5353))
            sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_JOIN_GROUP,
                            socket.inet_pton(socket.AF_INET6, 'ff02::fb') + struct.pack('@I', socket.if_nametoindex('eth0')))
        print('multicast listener ready family='+str(family), flush=True)
        while True:
            data, sender = sock.recvfrom(4096)
            if b'clankerd-evidence-' in data:
                with open('/tmp/multicast-received.jsonl', 'a') as output:
                    output.write(json.dumps({'family': family, 'sender': sender, 'dns_packet_hex': data.hex()}) + '\n')
                label = os.environ.get('EVIDENCE_NAME', '')
                question = dns_question(label)
                if family == socket.AF_INET and label and data[2:4] == b'\0\0' and data[12:] == question + struct.pack('!2H', 1, 1):
                    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as route:
                        route.connect(sender)
                        address = route.getsockname()[0]
                    answer = question + struct.pack('!2HIH', 1, 1, 10, 4) + socket.inet_aton(address)
                    response = data[:2] + struct.pack('!5H', 0x8400, 1, 1, 0, 0) + data[12:] + answer
                    sock.sendto(response, sender)
    for family in (socket.AF_INET, socket.AF_INET6):
        threading.Thread(target=receive, args=(family,), daemon=True).start()


def multicast_send(label):
    name = ('clankerd-evidence-' + label + '.local').encode()
    question = b''.join(bytes([len(part)]) + part for part in name.split(b'.')) + b'\0'
    packet = struct.pack('!6H', 0, 0, 1, 0, 0, 0) + question + struct.pack('!2H', 1, 1)
    for family, group in ((socket.AF_INET, '224.0.0.251'), (socket.AF_INET6, 'ff02::fb')):
        with socket.socket(family, socket.SOCK_DGRAM) as sock:
            if family == socket.AF_INET:
                sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF,
                                socket.inet_aton('0.0.0.0') + socket.inet_aton('0.0.0.0') + struct.pack('@I', socket.if_nametoindex('eth0')))
                sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 255)
                endpoint = (group, 5353)
            else:
                index = socket.if_nametoindex('eth0')
                sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_IF, index)
                sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_HOPS, 255)
                endpoint = (group, 5353, 0, index)
            for _ in range(3):
                sock.sendto(packet, endpoint)
                time.sleep(0.2)


def dns_question(label):
    name = ('clankerd-evidence-' + label + '.local').encode()
    return b''.join(bytes([len(part)]) + part for part in name.split(b'.')) + b'\0'


def discover(label, address, expected, mac_interface_address=None):
    question = dns_question(label)
    packet = struct.pack('!6H', 0, 0, 1, 0, 0, 0) + question + struct.pack('!2H', 1, 1)
    found = False
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(2)
        interface = (socket.inet_aton(mac_interface_address) if mac_interface_address else
                     socket.inet_aton('0.0.0.0') * 2 + struct.pack('@I', socket.if_nametoindex('eth0')))
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, interface)
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 255)
        for _ in range(2):
            sock.sendto(packet, ('224.0.0.251', 5353))
            try:
                reply, sender = sock.recvfrom(4096)
                found = (sender[1] == 5353 and reply[2:4] == b'\x84\0' and reply.endswith(socket.inet_aton(address)) and question in reply)
            except socket.timeout:
                pass
            if found:
                break
    print('mDNS A discovery ' + label + ': ' + ('resolved ' + address if found else 'no reply'), flush=True)
    assert found == expected, (label, address, found, expected)
