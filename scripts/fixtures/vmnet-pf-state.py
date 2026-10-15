#!/usr/bin/env python3
"""Owned IPv4 UDP tuple spoof probe and independent TCP/UDP held flows."""
import ipaddress
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import threading
import time


def record(path, **row):
    with open(path, 'a') as output:
        output.write(json.dumps(dict(time=time.time(), **row))+'\n')


def paths(nonce, role):
    assert nonce.isalnum(), nonce
    return Path('/tmp/pf-state-'+nonce+'-'+role+'.jsonl')


def server(nonce):
    path=paths(nonce,'server')
    def udp():
        with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as sock:
            sock.bind(('0.0.0.0',19092))
            record(path,ready='UDP')
            while True:
                payload,peer=sock.recvfrom(4096)
                record(path,protocol='UDP',peer=peer,payload=payload.decode(errors='replace'))
                sock.sendto(payload,peer)
    def connection(conn,peer):
        with conn:
            while data:=conn.recv(4096):
                record(path,protocol='TCP',peer=peer,payload=data.decode(errors='replace'))
                conn.sendall(data)
    threading.Thread(target=udp,daemon=True).start()
    with socket.socket() as sock:
        sock.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
        sock.bind(('0.0.0.0',19091));sock.listen()
        record(path,ready='TCP')
        while True:
            conn,peer=sock.accept()
            threading.Thread(target=connection,args=(conn,peer),daemon=True).start()


def hold(nonce,target,source,tcp_port,udp_port):
    path=paths(nonce,'client')
    with socket.socket() as tcp,socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as udp:
        tcp.settimeout(2);udp.settimeout(2)
        tcp.bind((source,int(tcp_port)));tcp.connect((target,19091))
        udp.bind((source,int(udp_port)));udp.connect((target,19092))
        for sequence in range(300):
            payload=f'pf-state:{nonce}:legitimate:{sequence}'.encode()
            try:
                tcp.sendall(payload)
                reply=bytearray()
                while len(reply)<len(payload):
                    data=tcp.recv(4096)
                    if not data:raise RuntimeError('TCP EOF')
                    reply+=data
                assert reply==payload,(reply,payload)
                udp.send(payload)
                deadline=time.monotonic()+2
                while time.monotonic()<deadline:
                    reply=udp.recv(4096)
                    if reply==payload:break
                    record(path,unexpected_echo=reply.decode(errors='replace'))
                assert reply==payload,(reply,payload)
                record(path,healthy=True,sequence=sequence,tcp=True,udp=True)
            except Exception as error:
                record(path,healthy=False,error=str(error));raise
            time.sleep(.3)


def checksum(data):
    if len(data)%2:data+=b'\0'
    value=sum(struct.unpack('!'+str(len(data)//2)+'H',data))
    while value>>16:value=(value&65535)+(value>>16)
    return (~value)&65535


def frame(source_mac,gateway_mac,source_ip,target_ip,port,payload):
    source=socket.inet_aton(source_ip);target=socket.inet_aton(target_ip)
    length=8+len(payload)
    udp=struct.pack('!HHHH',int(port),19092,length,0)
    pseudo=source+target+struct.pack('!BBH',0,17,length)
    udp=struct.pack('!HHHH',int(port),19092,length,checksum(pseudo+udp+payload) or 65535)+payload
    header=struct.pack('!BBHHHBBH4s4s',0x45,0,20+len(udp),0x7110,0,64,17,0,source,target)
    header=header[:10]+struct.pack('!H',checksum(header))+header[12:]
    return gateway_mac+source_mac+b'\x08\x00'+header+udp


def inject(nonce,source,target,port,label):
    assert ipaddress.ip_address(source).version==ipaddress.ip_address(target).version==4
    destination=int.from_bytes(socket.inet_aton(target),'little')
    routes=[]
    for row in Path('/proc/net/route').read_text().splitlines()[1:]:
        fields=row.split();mask=int(fields[7],16)
        if fields[0]=='eth0' and int(fields[3],16)&1 and destination&mask==int(fields[1],16):
            routes.append((mask.bit_count(),int(fields[2],16)))
    assert routes,'No eth0 route to owned destination'
    gateway_value=max(routes)[1]
    gateway=socket.inet_ntoa(gateway_value.to_bytes(4,'little')) if gateway_value else target
    # Actual native next-hop only; packet destination remains the owned peer.
    with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as primer:
        primer.sendto(b'pf-state-neighbor-primer',(gateway,18082))
    deadline=time.monotonic()+5
    while True:
        neighbors=[line.split() for line in Path('/proc/net/arp').read_text().splitlines()[1:]]
        neighbor=next((row for row in neighbors if row[0]==gateway and row[5]=='eth0' and int(row[2],16)&2),None)
        if neighbor:break
        assert time.monotonic()<deadline,'Gateway neighbor unresolved'
        time.sleep(.2)
    mac=bytes.fromhex(Path('/sys/class/net/eth0/address').read_text().strip().replace(':',''))
    next_mac=bytes.fromhex(neighbor[3].replace(':',''))
    payload=f'pf-state:{nonce}:{label}'.encode()
    packet=frame(mac,next_mac,source,target,port,payload)
    with socket.socket(socket.AF_PACKET,socket.SOCK_RAW,socket.htons(0x0800)) as sock:
        sock.bind(('eth0',0))
        for _ in range(3):
            assert sock.send(packet)==len(packet)
            time.sleep(.15)
    print(json.dumps(dict(injected=True,label=label,source=source,target=target,source_port=int(port),
                          destination_port=19092,source_mac=mac.hex(':'),gateway=gateway,
                          gateway_mac=next_mac.hex(':'),payload=payload.decode(),frame_hex=packet.hex())),flush=True)


def main():
    mode,*args=sys.argv[1:]
    if mode=='launch':
        source,role,nonce,*rest=args
        log=Path('/tmp/pf-state-'+nonce+'-'+role+'.log').open('w')
        process=subprocess.Popen([sys.executable,'-c',source,role,nonce,*rest],stdout=log,stderr=log,
                                 stdin=subprocess.DEVNULL,start_new_session=True)
        print(process.pid)
    elif mode=='server':server(*args)
    elif mode=='hold':hold(*args)
    elif mode=='inject':inject(*args)
    elif mode=='read':print(paths(*args).read_text(),end='')
    else:raise ValueError(mode)


if __name__=='__main__':main()
