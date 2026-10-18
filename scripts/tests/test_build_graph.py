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
        host = archive('host.tar.gz', {'libkrun/1.19.6/lib/libkrun.1.19.6.dylib': dylib, 'xz/5.8.4/lib/liblzma.5.dylib': dylib})
        self.lock = self.root / 'inputs.mk'
        self.lock.write_text('INPUTS := base host\nGUEST_INPUTS := base\nHOST_INPUTS := host\n' +
            ''.join(f'{name}_URL := {path.as_uri()}\n{name}_SHA256 := {hashlib.sha256(path.read_bytes()).hexdigest()}\n'
                    for name, path in [('base', base), ('host', host)]))
        tools = self.root / 'tools'
        tools.mkdir()
        cargo = tools / 'cargo'
        cargo.write_text('''#!/usr/bin/env python3
import os,sys,struct
from pathlib import Path
args=sys.argv
if '--version' in args: print('fixture cargo'); sys.exit(0)
kind='guest' if 'aarch64-unknown-linux-musl' in args else 'host'
assert not any(name in os.environ for name in ('MAKEFLAGS', 'MFLAGS', 'CARGO_MAKEFLAGS'))
if kind=='host':
 metadata=Path(os.environ['PKG_CONFIG_LIBDIR'])/'liblzma.pc'
 assert metadata.is_file()
 assert (metadata.parent.parent/'lib/liblzma.dylib').is_file()
 assert os.environ['PKG_CONFIG_ALLOW_CROSS']=='1'
 assert os.environ['PKG_CONFIG_PATH']==str(metadata.parent)
 assert os.environ['PKG_CONFIG_SYSROOT_DIR']=='/'
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
  (out/name).write_text('compiled guest')
 else:
  commands=b''
  if name=='clankerd-vmspawn':
   def command(code,header,value):
    data=value.encode()+bytes([0]);size=(header+len(data)+7)//8*8
    return struct.pack('<III',code,size,header)+bytes(header-12)+data+bytes(size-header-len(data))
   commands=command(0xc,24,'@rpath/libkrun.1.dylib')+command(0x8000001c,12,'@executable_path/lib')
  (out/name).write_bytes(struct.pack('<8I',0xfeedfacf,0x100000c,0,2,2 if commands else 0,len(commands),0,0)+commands)
''')
        cargo.chmod(0o755)
        signer = tools / 'rcodesign'
        signer.write_text('#!/bin/sh\nif [ "$1" = --version ]; then echo fixture; else [ -z "$FAIL_SIGN" ]; fi\n')
        signer.chmod(0o755)
        for name in ['rustc', 'zig']:
            tool = tools / name
            tool.write_text('#!/bin/sh\necho fixture\n')
            tool.chmod(0o755)
        self.log = self.root / 'cargo.log'
        self.env = dict(os.environ, PATH=str(tools)+os.pathsep+os.environ['PATH'], GRAPH_LOG=str(self.log))
        self.args = ['make', '--no-print-directory', '-j4', 'build', f'INPUT_LOCK={self.lock}']

    def run_make(self, ok=True, env=None):
        result = subprocess.run(self.args, cwd=self.root, env=env or self.env, text=True, capture_output=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout+result.stderr)
        return result

    def test_build_works_without_the_loader_api_removed_in_python_315(self):
        startup = self.root / 'python315'
        startup.mkdir()
        (startup / 'sitecustomize.py').write_text(
            'from importlib.machinery import SourceFileLoader\n'
            'for cls in SourceFileLoader.__mro__:\n'
            '    if "load_module" in vars(cls): delattr(cls, "load_module")\n')
        self.run_make(env=dict(self.env, PYTHONPATH=str(startup)))
        self.assertTrue((self.root / 'build/rust/darwin-arm64/vmctl').is_file())

    def test_warm_build_missing_output_and_changed_rust_have_the_correct_rebuild_scope(self):
        self.run_make()
        artifact = self.root / 'build/rust'
        self.assertTrue((artifact / 'linux-arm64/management/sbin/dhcpcd').is_file())
        original = artifact.resolve()
        calls = self.log.read_text()
        self.run_make()
        self.assertEqual(self.log.read_text(), calls)
        self.assertEqual(artifact.resolve(), original)
        (artifact / 'linux-arm64/management/sbin/dhcpcd').unlink()
        self.run_make()
        self.assertTrue((artifact / 'linux-arm64/management/sbin/dhcpcd').is_file())
        self.assertEqual(self.log.read_text(), calls)
        (self.root / 'crates/libclankerd/src/lib.rs').write_text('changed host source')
        self.run_make()
        self.assertEqual(self.log.read_text(), calls+'host\n')
        calls = self.log.read_text()
        (self.root / 'crates/libclankerd/src/lib.rs').unlink()
        self.run_make()
        self.assertEqual(self.log.read_text(), calls+'host\n')
        calls = self.log.read_text()
        restored = self.root / 'crates/libclankerd/src/lib.rs'
        restored.write_text('restored source')
        os.utime(restored, ns=(1, 1))
        self.run_make()
        self.assertEqual(self.log.read_text(), calls+'host\n')

    def test_legacy_bundle_is_retained_when_the_complete_artifact_is_published(self):
        output = self.root / 'build/rust'
        (output / 'darwin-arm64').mkdir(parents=True)
        (output / 'linux-arm64').mkdir()
        (output / 'darwin-arm64/vmctl').write_text('previous bundle')
        self.run_make()
        self.assertTrue(output.is_symlink())
        previous = list((self.root / 'build').glob('.rust-legacy-*'))
        self.assertEqual(len(previous), 1)
        self.assertEqual((previous[0] / 'darwin-arm64/vmctl').read_text(), 'previous bundle')

    def test_failed_signing_and_corrupt_input_preserve_the_published_artifact(self):
        self.run_make()
        artifact = self.root / 'build/rust'
        original = artifact.resolve()
        (self.root / 'crates/libclankerd/src/lib.rs').write_text('changed host source')
        self.run_make(ok=False, env=dict(self.env, FAIL_SIGN='1'))
        self.assertEqual(artifact.resolve(), original)
        self.assertTrue((artifact / 'darwin-arm64/vmctl').read_bytes().startswith(bytes.fromhex('cffaedfe')))
        archive = next((self.root / 'build/self-contained/downloads').glob('base-*.archive'))
        archive.write_bytes(b'corrupt')
        self.run_make(ok=False)
        self.assertEqual(artifact.resolve(), original)

    def test_changed_guest_source_and_declared_binary_rebuild_only_affected_outputs(self):
        import gzip
        self.run_make()
        calls = self.log.read_text()
        (self.root / 'crates/clankerd-guestd/src/main.rs').write_text('changed guest')
        self.run_make()
        self.assertEqual(self.log.read_text(), calls+'guest\n')
        calls = self.log.read_text()
        package = self.source / 'base.tar.gz'
        old = hashlib.sha256(package.read_bytes()).hexdigest()
        package.write_bytes(gzip.compress(gzip.decompress(package.read_bytes()), mtime=123))
        new = hashlib.sha256(package.read_bytes()).hexdigest()
        self.lock.write_text(self.lock.read_text().replace(old, new))
        previous = (self.root / 'build/rust').resolve()
        self.run_make()
        self.assertEqual(self.log.read_text(), calls)
        self.assertNotEqual((self.root / 'build/rust').resolve(), previous)
        self.assertIn(new, (self.root / 'build/rust/inputs.mk').read_text())

    def test_concurrent_builds_and_interrupted_compilation_never_publish_partial_outputs(self):
        import signal
        import time
        with subprocess.Popen(self.args, cwd=self.root, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT) as first, \
             subprocess.Popen(self.args, cwd=self.root, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT) as second:
            results = [(process, process.communicate(timeout=30)[0]) for process in [first, second]]
            for process, output in results:
                self.assertEqual(process.returncode, 0, output.decode())
        artifact = self.root / 'build/rust'
        original = artifact.resolve()
        (self.root / 'crates/libclankerd/src/lib.rs').write_text('recompile')
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
        self.assertEqual(artifact.resolve(), original)
        self.run_make()
        self.assertNotEqual(artifact.resolve(), original)

    def test_an_incorrect_declared_hash_cannot_publish_an_archive_or_artifact(self):
        self.run_make()
        original = (self.root / 'build/rust').resolve()
        archive = self.source / 'base.tar.gz'
        old = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.lock.write_text(self.lock.read_text().replace(old, 'f'*64))
        self.run_make(ok=False)
        self.assertFalse(list((self.root / 'build/self-contained/downloads').glob('base-'+'f'*64+'.archive')))
        self.assertEqual((self.root / 'build/rust').resolve(), original)

if __name__ == '__main__':
    unittest.main()
