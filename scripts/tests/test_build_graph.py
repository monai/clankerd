"""Exercise the real Make entry point using a local binary input."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BinaryInputs(unittest.TestCase):
    def test_verified_input_is_reused_and_corruption_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            source = work / 'binary.tar.gz'
            source.write_bytes(b'pinned binary fixture\n')
            lock = work / 'inputs.mk'
            lock.write_text('INPUTS := fixture\nfixture_URL := ' + source.as_uri() +
                            '\nfixture_SHA256 := ' + hashlib.sha256(source.read_bytes()).hexdigest() + '\n')
            cache = work / 'cache'
            command = ['make', '--no-print-directory', 'verify-inputs',
                       f'INPUT_LOCK={lock}', f'BUILD_ROOT={work / "build"}', f'DOWNLOADS={cache}']
            def run():
                return subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
            first = run()
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            archive = next(cache.glob('*.archive'))
            previous = archive.stat().st_mtime_ns
            source.unlink()  # A warm build cannot depend on a live server.
            self.assertEqual(run().returncode, 0)
            self.assertEqual(archive.stat().st_mtime_ns, previous)
            archive.write_bytes(b'corrupt')
            failed = run()
            self.assertNotEqual(failed.returncode, 0, failed.stdout + failed.stderr)


class ArtifactGraph(unittest.TestCase):
    def setUp(self):
        import io
        import shutil
        import struct
        import tarfile
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        shutil.copy2(ROOT / 'Makefile', self.root / 'Makefile')
        shutil.copytree(ROOT / 'scripts', self.root / 'scripts', ignore=shutil.ignore_patterns('__pycache__'))
        shutil.copytree(ROOT / 'build-support', self.root / 'build-support')
        for name in ['Cargo.toml', 'Cargo.lock', 'crates/clankerd-vmspawn/entitlements.plist',
                     'crates/clankerd-guestd/src/main.rs', 'crates/libclankerd/src/lib.rs']:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('fixture')
        self.source = self.root / 'inputs'
        self.source.mkdir()
        def archive(name, files):
            path = self.source / name
            with tarfile.open(path, 'w:gz') as tar:
                for name, data in files.items():
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    entry.mode = 0o755
                    tar.addfile(entry, io.BytesIO(data))
            return path
        base = archive('base.tar.gz', {'etc/passwd': b'root:x:0:0:root:/root:/bin/sh\n',
            'etc/group': b'root:x:0:\n', 'bin/sh': b'shell', 'sbin/dhcpcd': b'dhcp',
            'sbin/mke2fs': b'format', 'usr/sbin/resize2fs': b'grow', 'lib/ld-musl-aarch64.so.1': b'loader',
            'lib/apk/db/installed': b'fixture package database'})
        dylib = struct.pack('<8I', 0xfeedfacf, 0x100000c, 0, 6, 0, 0, 0, 0)
        host = archive('host.tar.gz', {'libkrun/1.19.6/lib/libkrun.1.19.6.dylib': dylib, 'xz/5.8.4/lib/liblzma.5.dylib': dylib, 'xz/5.8.4/lib/pkgconfig/liblzma.pc': b'Version: 5.8.4\n'})
        self.lock = self.root / 'inputs.mk'
        self.lock.write_text('INPUTS := base host\nGUEST_INPUTS := base\nHOST_INPUTS := host\n' +
            ''.join(f'{name}_URL := {path.as_uri()}\n{name}_SHA256 := {hashlib.sha256(path.read_bytes()).hexdigest()}\n'
                    for name, path in [('base', base), ('host', host)]))
        tools = self.root / 'tools'
        tools.mkdir()
        cargo = tools / 'cargo'
        cargo.write_text('''#!/usr/bin/env python3
import os,sys,struct,re,subprocess
from pathlib import Path
args=sys.argv
for flags in (os.environ.get(name,'') for name in ('MAKEFLAGS','CARGO_MAKEFLAGS')):
 for low,high in re.findall(r'--jobserver-(?:fds|auth)=(\\d+),(\\d+)',flags):
  os.fstat(int(low)); os.fstat(int(high))
if '--version' in args:
 print('fixture cargo'); sys.exit(0)
kind='guest' if 'aarch64-unknown-linux-musl' in args else 'host'
if kind=='host':
 libdir=Path(subprocess.check_output(['pkg-config','--variable=libdir','liblzma'],text=True).strip())
 expected=next(Path('build/work').glob('*/host-libraries/lib/liblzma.5.dylib'))
 assert (libdir/'liblzma.dylib').resolve()==expected.resolve()
 assert (libdir/'liblzma.dylib').is_file()
with open(os.environ['GRAPH_LOG'],'a') as stream: stream.write(kind+'\\n')
if os.environ.get('FAIL_CARGO'): sys.exit(1)
if os.environ.get('BLOCK_CARGO'):
 import time
 Path(os.environ['BLOCK_CARGO']).write_text('compiling')
 time.sleep(30)
out=Path(os.environ['CARGO_TARGET_DIR'])/args[args.index('--target')+1]/'release'
out.mkdir(parents=True,exist_ok=True)
for name in (['clankerd-guestd'] if kind=='guest' else ['vmctl','clankerd-vmspawn','clankerd-network']):
 if kind=='guest':
  data=b'compiled guest'+os.environ.get('GUEST_PAYLOAD','').encode()
 else:
  commands=b''
  if name=='clankerd-vmspawn':
   def command(code,header,value):
    data=value.encode()+bytes([0]);size=(header+len(data)+7)//8*8
    return struct.pack('<III',code,size,header)+bytes(header-12)+data+bytes(size-header-len(data))
   commands=command(0xc,24,'@rpath/libkrun.1.dylib')+command(0x8000001c,12,'@executable_path/lib')
  data=struct.pack('<8I',0xfeedfacf,0x100000c,0,2,2 if commands else 0,len(commands),0,0)+commands+os.environ.get('HOST_PAYLOAD','').encode()
 if not (out/name).exists() or (out/name).read_bytes()!=data:
  temporary=out/(name+'.'+str(os.getpid()))
  temporary.write_bytes(data)
  temporary.chmod(0o755)
  temporary.replace(out/name)
  if os.environ.get('CARGO_MTIME_NS'):
   timestamp=int(os.environ['CARGO_MTIME_NS'])
   os.utime(out/name, ns=(timestamp,timestamp))
''')
        cargo.chmod(0o755)
        signer = tools / 'rcodesign'
        signer.write_text('#!/bin/sh\n'
                          'if [ -n "$GRAPH_SIGN_LOG" ]; then printf "%s\\n" "$*" >> "$GRAPH_SIGN_LOG"; fi\n'
                          'if [ "$1" = --version ]; then echo fixture; else [ -z "$FAIL_SIGN" ]; fi\n')
        signer.chmod(0o755)
        self.log = self.root / 'cargo.log'
        self.env = dict(os.environ, PATH=str(tools)+os.pathsep+os.environ['PATH'], GRAPH_LOG=str(self.log))
        self.args = ['make', '--no-print-directory', '-j4', 'build', f'INPUT_LOCK={self.lock}',
                     'BUILD_ROOT=build/work', 'DOWNLOADS=build/work/downloads',
                     f'CARGO_TARGET_DIR={self.root / "cargo"}']

    def run_make(self, ok=True, env=None):
        result = subprocess.run(self.args, cwd=self.root, env=env or self.env, text=True, capture_output=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout+result.stderr)
        return result

    def artifact_state(self):
        root = self.root / 'build/artifact'
        return {str(path.relative_to(root)): (path.lstat().st_mtime_ns,
                os.readlink(path) if path.is_symlink() else path.read_bytes())
                for path in root.rglob('*') if path.is_file() or path.is_symlink()}

    def test_subsecond_cargo_update_is_installed_and_warm_build_does_not_sign(self):
        import time
        self.run_make()
        installed = self.root / 'build/artifact/darwin-arm64/vmctl'
        now = time.time_ns()
        timestamp = now - (now % 1_000_000_000) // 2
        os.utime(installed, ns=(timestamp, timestamp))
        signing_log = self.root / 'signing.log'
        environment = dict(self.env, HOST_PAYLOAD='subsecond update',
                           CARGO_MTIME_NS=str(timestamp + 1), GRAPH_SIGN_LOG=str(signing_log))
        self.run_make(env=environment)
        source = self.root / 'cargo/aarch64-apple-darwin/release/vmctl'
        self.assertEqual(source.stat().st_mtime_ns, timestamp + 1)
        self.assertEqual(installed.read_bytes(), source.read_bytes())
        self.assertIn('io.clankerd.vmctl', signing_log.read_text())
        before = self.artifact_state()
        signing_log.write_text('')
        self.run_make(env=environment)
        self.assertEqual(self.artifact_state(), before)
        self.assertEqual(signing_log.read_text(), '')

    def test_cargo_is_consulted_and_unchanged_executables_reuse_the_artifact(self):
        self.run_make()
        artifact = self.root / 'build/artifact'
        management = artifact / 'linux-arm64/management'
        self.assertEqual({p.name for p in (artifact / 'linux-arm64').iterdir()}, {'management'})
        self.assertTrue((management / 'usr/sbin/clankerd-guestd').is_file())
        self.assertFalse(any(p.is_file() or p.is_symlink() for p in management.iterdir()))
        original = self.artifact_state()
        self.run_make()
        self.assertEqual(self.artifact_state(), original)
        calls = self.log.read_text().splitlines()
        self.assertEqual(sorted(calls), ['guest', 'guest', 'host', 'host'])
        source = self.root / 'crates/libclankerd/src/unused.rs'
        source.write_text('fixture source addition')
        self.run_make()
        source.unlink()
        self.run_make()
        self.assertEqual(self.artifact_state(), original,
                         'Cargo outputs, rather than source inventory, determine repackaging')
        executable = self.root / 'cargo/aarch64-apple-darwin/release/vmctl'
        executable.unlink()
        self.run_make()
        self.assertTrue(executable.is_file())
        (management / 'sbin/dhcpcd').unlink()
        self.run_make()
        self.assertTrue((management / 'sbin/dhcpcd').is_file())
        self.assertFalse(list((self.root / 'build/work').glob('*/host-inputs.json')))
        self.assertFalse(list((self.root / 'build/work').glob('*/guest-inputs.json')))

    def test_static_management_inputs_rebuild_the_filesystem_without_recompiling(self):
        self.run_make()
        artifact = self.root / 'build/artifact'
        config = self.root / 'build-support/management-rootfs/etc/clankerd/dhcpcd.conf'
        config.write_text('duid ll\nnoipv4ll\n# fixture policy\n')
        self.run_make()
        self.assertEqual((artifact / 'linux-arm64/management/etc/clankerd/dhcpcd.conf').read_text(),
                         'duid ll\nnoipv4ll\n# fixture policy\n')

    def test_signed_test_libraries_do_not_require_rust_or_guest_artifacts(self):
        import platform
        context = platform.system() + '-' + platform.machine()
        self.args[3] = 'test-libraries'
        self.run_make()
        work = self.root / 'build/work' / context
        self.assertTrue((work / 'test-libraries/darwin-arm64/lib/libkrun.1.dylib').is_file())
        self.assertFalse(self.log.exists())
        self.assertFalse((work / 'management').exists())
        self.assertFalse((self.root / 'build/artifact').exists())

    def test_build_installs_into_an_ordinary_directory(self):
        output = self.root / 'build/artifact'
        output.mkdir(parents=True)
        (output / 'user-file').write_text('fixture user data')
        self.run_make()
        self.assertFalse(output.is_symlink())
        self.assertEqual((output / 'user-file').read_text(), 'fixture user data')

    def test_failed_build_stages_preserve_the_published_artifact(self):
        self.run_make()
        artifact = self.root / 'build/artifact'
        original = self.artifact_state()
        self.run_make(ok=False, env=dict(self.env, FAIL_CARGO='1'))
        self.assertEqual(self.artifact_state(), original)
        self.run_make(ok=False, env=dict(self.env, HOST_PAYLOAD='changed host', FAIL_SIGN='1'))
        self.assertEqual(self.artifact_state(), original)
        self.assertTrue((artifact / 'darwin-arm64/vmctl').read_bytes().startswith(bytes.fromhex('cffaedfe')))
        archive = next((self.root / 'build/work/downloads').glob('base-*.archive'))
        archive.write_bytes(b'corrupt')
        self.run_make(ok=False)
        self.assertEqual(self.artifact_state(), original)

    def test_changed_cargo_output_and_locked_package_update_the_artifact(self):
        import gzip
        self.run_make()
        artifact = self.root / 'build/artifact'
        original = self.artifact_state()
        host = (artifact / 'darwin-arm64/vmctl').read_bytes()
        env = dict(self.env, GUEST_PAYLOAD='changed guest')
        self.run_make(env=env)
        self.assertNotEqual(self.artifact_state(), original)
        self.assertEqual((artifact / 'darwin-arm64/vmctl').read_bytes(), host)
        self.assertEqual((artifact / 'linux-arm64/management/usr/sbin/clankerd-guestd').read_bytes(),
                         b'compiled guestchanged guest')
        package = self.source / 'base.tar.gz'
        old = hashlib.sha256(package.read_bytes()).hexdigest()
        package.write_bytes(gzip.compress(gzip.decompress(package.read_bytes()), mtime=123))
        new = hashlib.sha256(package.read_bytes()).hexdigest()
        self.lock.write_text(self.lock.read_text().replace(old, new))
        previous = self.artifact_state()
        self.run_make(env=env)
        self.assertNotEqual(self.artifact_state(), previous)
        self.assertIn(new, (artifact / 'inputs.mk').read_text())

    def test_interrupted_compilation_does_not_replace_installed_outputs(self):
        import signal
        import time
        self.run_make()
        artifact = self.root / 'build/artifact'
        original = self.artifact_state()
        marker = self.root / 'compiling'
        process = subprocess.Popen(self.args, cwd=self.root, env=dict(self.env, BLOCK_CARGO=str(marker)),
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic()+10
            while not marker.exists() and process.poll() is None and time.monotonic()<deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists())
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            process.communicate(timeout=10)
        self.assertEqual(self.artifact_state(), original)
        self.run_make(env=dict(self.env, HOST_PAYLOAD="completed host"))
        self.assertNotEqual(self.artifact_state(), original)

    def test_an_incorrect_declared_hash_cannot_publish_an_archive_or_artifact(self):
        self.run_make()
        original = self.artifact_state()
        archive = self.source / 'base.tar.gz'
        old = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.lock.write_text(self.lock.read_text().replace(old, 'f'*64))
        self.run_make(ok=False)
        self.assertFalse(list((self.root / 'build/work/downloads').glob('base-'+'f'*64+'.archive')))
        self.assertEqual(self.artifact_state(), original)

if __name__ == '__main__':
    unittest.main()
