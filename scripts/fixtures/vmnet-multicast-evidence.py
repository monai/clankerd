import json
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
