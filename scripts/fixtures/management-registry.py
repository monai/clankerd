#!/usr/bin/env python3
"""Tiny read-only OCI fixture registry: no container daemon or image builder."""
import gzip, hashlib, io, json, tarfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

def start(work, probe, log):
    objects={}
    def blob(data):
        digest='sha256:'+hashlib.sha256(data).hexdigest();objects[digest]=(data,'application/octet-stream');return digest
    buf=io.BytesIO()
    with tarfile.open(fileobj=buf,mode='w') as archive:
        for name in ['config','lib','volatile','var','var/lib','var/lib/dhcpcd','volatile/dhcpcd','root','mount-check']:
            item=tarfile.TarInfo(name);item.type=tarfile.DIRTYPE;item.mode=0o755;archive.addfile(item)
        for name, target in [('etc', '/config'), ('run', '/volatile'), ('.old-management-root', '/old-root-slot')]:
            item=tarfile.TarInfo(name);item.type=tarfile.SYMTYPE;item.linkname=target;item.mode=0o777;archive.addfile(item)
        for name,data,mode in [('probe',probe.read_bytes(),0o755),('config/passwd',b'root:x:0:0:hostile:/root:/missing\ndhcpcd:x:4242:4242:conflict:/hostile:/missing\n',0o644),('lib/ld-musl-aarch64.so.1',b'hostile workload loader\n',0o644),('config/dhcpcd.conf',b'noipv4\nnoipv6\n',0o644),('var/lib/dhcpcd/workload-only',b'conflict\n',0o644)]:
            item=tarfile.TarInfo(name);item.size=len(data);item.mode=mode;archive.addfile(item,io.BytesIO(data))
    raw=buf.getvalue();layer=gzip.compress(raw,mtime=0);layer_digest=blob(layer)
    config=json.dumps({'architecture':'arm64','os':'linux','config':{'Entrypoint':['/probe'],'Cmd':['sleep'],'WorkingDir':'/','User':'0:0'},'rootfs':{'type':'layers','diff_ids':['sha256:'+hashlib.sha256(raw).hexdigest()]}}).encode()
    cfg=blob(config)
    media='application/vnd.oci.image.manifest.v1+json'
    manifest=json.dumps({'schemaVersion':2,'mediaType':media,'config':{'mediaType':'application/vnd.oci.image.config.v1+json','digest':cfg,'size':len(config)},'layers':[{'mediaType':'application/vnd.oci.image.layer.v1.tar+gzip','digest':layer_digest,'size':len(layer)}]}).encode()
    digest=blob(manifest);objects[digest]=(manifest,media)
    class Handler(BaseHTTPRequestHandler):
        def do_HEAD(self): self.do_GET()
        def do_GET(self):
            if self.path.rstrip('/')=='/v2': body,ctype=b'{}','application/json'
            elif '/manifests/' in self.path: body,ctype=manifest,media
            elif '/blobs/' in self.path and self.path.rsplit('/',1)[-1] in objects: body,ctype=objects[self.path.rsplit('/',1)[-1]]
            else: self.send_error(404);return
            self.send_response(200);self.send_header('Content-Type',ctype);self.send_header('Content-Length',str(len(body)));self.send_header('Docker-Distribution-Api-Version','registry/2.0');self.send_header('Docker-Content-Digest','sha256:'+hashlib.sha256(body).hexdigest());self.end_headers()
            if self.command!='HEAD':self.wfile.write(body)
        def log_message(self,fmt,*args): log.write((fmt%args)+'\n');log.flush()
    server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
    (work/'scratch-image.json').write_text(json.dumps({'manifest_digest':digest,'config_digest':cfg,'layer_digest':layer_digest,'files':'static /probe; no shell; conflicting musl loader, dhcpcd account/config/state'},indent=2)+'\n')
    import threading
    threading.Thread(target=server.serve_forever,daemon=True).start()
    return server, f'127.0.0.1:{server.server_port}/experiment/scratch:proof'
