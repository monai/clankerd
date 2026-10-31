"""Exercise the CMake package and artifact graph with local external inputs."""
import hashlib
import os
import signal
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BinaryInputs(unittest.TestCase):
    def fixture(self, work, source, digest, download):
        import shutil
        project = work / 'project'
        project.mkdir()
        shutil.copytree(ROOT / 'cmake', project / 'cmake')
        lock = project / 'inputs.cmake'
        lock.write_text(f'set(INPUTS fixture)\nset(GUEST_INPUTS)\nset(LICENSE_INPUTS)\nset(fixture_URL "{source}")\nset(fixture_SHA256 {digest})\n')
        (project / 'CMakeLists.txt').write_text(f'''cmake_minimum_required(VERSION 3.25)
project(download_fixture NONE)
find_package(Python3 REQUIRED COMPONENTS Interpreter)
set(CLANKERD_SOURCE_DIR "${{CMAKE_SOURCE_DIR}}")
set(CLANKERD_WORK_DIR "${{CMAKE_BINARY_DIR}}/work")
set(CLANKERD_DOWNLOAD_DIR "{download}")
set(CLANKERD_INPUTS "{lock}")
include("${{CLANKERD_INPUTS}}")
include(cmake/Packages.cmake)
''')
        build = work / 'cmake'
        result = subprocess.run(['cmake', '-S', str(project), '-B', str(build), '-G', 'Ninja'], text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        return ['cmake', '--build', str(build), '--target', 'verify-inputs']

    def test_wrong_hash_leaves_no_published_archive_or_temporary_file(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            source = work / 'binary.tar.gz'
            source.write_bytes(b'wrong hash fixture\n')
            downloads = work / 'downloads'
            command = self.fixture(work, source.as_uri(), '0'*64, downloads)
            result = subprocess.run(command, text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0, result.stdout+result.stderr)
            self.assertEqual(list(downloads.iterdir()), [])

    def test_verified_input_is_reused_and_corruption_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            source = work / 'binary.tar.gz'
            source.write_bytes(b'pinned binary fixture\n')
            downloads = work / 'downloads'
            command = self.fixture(work, source.as_uri(), hashlib.sha256(source.read_bytes()).hexdigest(), downloads)
            first = subprocess.run(command, text=True, capture_output=True)
            self.assertEqual(first.returncode, 0, first.stdout+first.stderr)
            archive = next(downloads.glob('*.archive'))
            previous = archive.stat().st_mtime_ns
            source.unlink()
            warm = subprocess.run(command, text=True, capture_output=True)
            self.assertEqual(warm.returncode, 0, warm.stdout+warm.stderr)
            self.assertEqual(archive.stat().st_mtime_ns, previous)
            archive.write_bytes(b'corrupt')
            failed = subprocess.run(command, text=True, capture_output=True)
            self.assertNotEqual(failed.returncode, 0, failed.stdout+failed.stderr)

    def test_independent_build_processes_can_share_the_download_directory(self):
        self.check_concurrent_downloads(False)

    def test_failed_download_cannot_remove_another_process_verified_archive(self):
        self.check_concurrent_downloads(True)

    def check_concurrent_downloads(self, fail_one):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import threading
        payload = b'concurrent download fixture\n'
        digest = hashlib.sha256(payload).hexdigest()
        overlap = threading.Barrier(2)
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                overlap.wait(timeout=15)
                body = b'incorrect download\n' if self.path == '/bad' else payload
                self.send_response(200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            def log_message(self, *args):
                pass
        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            downloads = work / 'downloads'
            commands = []
            for index in range(2):
                root = work / str(index)
                root.mkdir()
                url = f'http://127.0.0.1:{server.server_port}/' + ('bad' if fail_one and index else 'good')
                commands.append(self.fixture(root, url, digest, downloads))
            processes = [subprocess.Popen(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
                         for command in commands]
            try:
                for index, process in enumerate(processes):
                    output = process.communicate(timeout=25)[0]
                    if fail_one and index == 1:
                        self.assertNotEqual(process.returncode, 0, output)
                    else:
                        self.assertEqual(process.returncode, 0, output)
                self.assertEqual((downloads / f'fixture-{digest}.archive').read_bytes(), payload)
                self.assertEqual(len(list(downloads.iterdir())), 1)
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.terminate()
                    process.communicate(timeout=10)


class ArtifactGraph(unittest.TestCase):
    def setUp(self):
        import io
        import shutil
        import struct
        import tarfile
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        shutil.copytree(ROOT / 'cmake', self.root / 'cmake')
        shutil.copytree(ROOT / 'scripts', self.root / 'scripts', ignore=shutil.ignore_patterns('__pycache__'))
        shutil.copytree(ROOT / 'crates/clankerd-guestd/rootfs', self.root / 'crates/clankerd-guestd/rootfs')
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
        self.lock = self.root / 'inputs.cmake'
        self.lock.write_text('set(INPUTS base)\nset(GUEST_INPUTS base)\nset(LICENSE_INPUTS)\nset(GUEST_REQUIRED bin/sh sbin/dhcpcd sbin/mke2fs usr/sbin/resize2fs lib/ld-musl-aarch64.so.1 etc/passwd etc/group)\n' +
            f'set(base_URL {base.as_uri()})\nset(base_SHA256 {hashlib.sha256(base.read_bytes()).hexdigest()})\n')
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
  data=struct.pack('<8I',0xfeedfacf,0x100000c,0,2,0,0,0,0)+os.environ.get('HOST_PAYLOAD','').encode()
  if name=='vmctl' and os.environ.get('INVALID_HOST'): data=b'invalid Mach-O'
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
        self.build_dir = self.root / 'build/cmake'
        (self.root / 'CMakeLists.txt').write_text(f"""cmake_minimum_required(VERSION 3.25)
project(graph_fixture NONE)
find_package(Python3 REQUIRED COMPONENTS Interpreter)
set(CLANKERD_SOURCE_DIR "${{CMAKE_SOURCE_DIR}}")
set(CLANKERD_WORK_DIR "${{CMAKE_SOURCE_DIR}}/build/work/fixture")
set(CLANKERD_DOWNLOAD_DIR "${{CMAKE_SOURCE_DIR}}/build/work/downloads")
set(CLANKERD_INPUTS "${{CMAKE_SOURCE_DIR}}/inputs.cmake")
set(CLANKERD_ARTIFACT_DIR "${{CMAKE_SOURCE_DIR}}/build/artifact")
set(CLANKERD_CARGO_TARGET_DIR "${{CMAKE_SOURCE_DIR}}/cargo")
set(CLANKERD_HOST_BINARY_DIR "${{CLANKERD_CARGO_TARGET_DIR}}/aarch64-apple-darwin/release")
set(CLANKERD_GUEST_BINARY "${{CLANKERD_CARGO_TARGET_DIR}}/aarch64-unknown-linux-musl/release/clankerd-guestd")
set(CLANKERD_SIGNER "${{CMAKE_SOURCE_DIR}}/tools/rcodesign")
include("${{CLANKERD_INPUTS}}")
include(cmake/Packages.cmake)
add_custom_target(cargo-host COMMAND "${{CMAKE_COMMAND}}" -E env "CARGO_TARGET_DIR=${{CLANKERD_CARGO_TARGET_DIR}}" "${{CMAKE_SOURCE_DIR}}/tools/cargo" build --target aarch64-apple-darwin)
add_custom_target(cargo-guest COMMAND "${{CMAKE_COMMAND}}" -E env "CARGO_TARGET_DIR=${{CLANKERD_CARGO_TARGET_DIR}}" "${{CMAKE_SOURCE_DIR}}/tools/cargo" build --target aarch64-unknown-linux-musl)
include(cmake/Artifact.cmake)
""")
        configured = subprocess.run(['cmake', '-S', str(self.root), '-B', str(self.build_dir), '-G', 'Ninja'],
                                    env=self.env, text=True, capture_output=True)
        self.assertEqual(configured.returncode, 0, configured.stdout+configured.stderr)
        self.args = ['cmake', '--build', str(self.build_dir), '--target', 'build', '--parallel', '4']

    def run_build(self, ok=True, env=None):
        with subprocess.Popen(self.args, cwd=self.root, env=env or self.env, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              start_new_session=True) as process:
            try:
                stdout, stderr = process.communicate(timeout=60)
            except subprocess.TimeoutExpired:
                processes = subprocess.run(['ps', '-axo', 'pid,ppid,pgid,state,command'],
                                           text=True, capture_output=True)
                evidence = '\n'.join(line for line in processes.stdout.splitlines()
                                     if len(line.split(None, 4)) >= 3
                                     and line.split(None, 4)[2] == str(process.pid))
                os.killpg(process.pid, signal.SIGKILL)
                stdout, stderr = process.communicate()
                self.fail(f'Build timed out. Build process group {process.pid}\n'
                          f'{stdout}\n{stderr}\nProcess evidence\n{evidence}')
            result = subprocess.CompletedProcess(self.args, process.returncode, stdout, stderr)
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

    def test_valid_archive_tampering_is_rejected_before_extraction(self):
        import io
        import tarfile
        self.run_build()
        original = self.artifact_state()
        archive = next((self.root / 'build/work/downloads').glob('base-*.archive'))
        with tarfile.open(archive, 'r:gz') as stream:
            members = [(member, stream.extractfile(member).read()) for member in stream.getmembers()]
        with tarfile.open(archive, 'w:gz') as stream:
            for entry, data in members:
                if entry.name == 'etc/passwd':
                    data = b'tampered user database\n'
                    entry.size = len(data)
                stream.addfile(entry, io.BytesIO(data))
        self.run_build(ok=False)
        self.assertEqual(self.artifact_state(), original)

    def test_removed_management_policy_file_is_removed_from_the_bundle(self):
        policy = self.root / 'crates/clankerd-guestd/rootfs/etc/obsolete-policy'
        policy.write_text('temporary policy\n')
        self.run_build()
        installed = self.root / 'build/artifact/linux-arm64/management/etc/obsolete-policy'
        self.assertTrue(installed.exists())
        policy.unlink()
        self.run_build()
        self.assertFalse(installed.exists())

    def test_permission_only_policy_change_is_published(self):
        policy = self.root / 'crates/clankerd-guestd/rootfs/etc/private-policy'
        policy.write_text('private policy\n')
        policy.chmod(0o644)
        self.run_build()
        installed = self.root / 'build/artifact/linux-arm64/management/etc/private-policy'
        self.assertEqual(installed.stat().st_mode & 0o777, 0o644)
        policy.chmod(0o600)
        stamp = next((self.root / 'build/work').glob('*/management/.stamp'))
        stamp.unlink()
        self.run_build()
        self.assertEqual(installed.stat().st_mode & 0o777, 0o600)

    def test_host_artifact_contains_only_the_three_executables(self):
        self.run_build()
        host = self.root / 'build/artifact/darwin-arm64'
        self.assertEqual({path.name for path in host.iterdir()},
                         {'vmctl', 'clankerd-vmspawn', 'clankerd-network'})

    def test_subsecond_cargo_update_is_installed_and_warm_build_does_not_sign(self):
        import time
        self.run_build()
        installed = self.root / 'build/artifact/darwin-arm64/vmctl'
        staged = next((self.root / 'build/work').glob('*/artifact/darwin-arm64/vmctl'))
        now = time.time_ns()
        timestamp = now - (now % 1_000_000_000) // 2
        os.utime(staged, ns=(timestamp, timestamp))
        signing_log = self.root / 'signing.log'
        environment = dict(self.env, HOST_PAYLOAD='subsecond update',
                           CARGO_MTIME_NS=str(timestamp + 1), GRAPH_SIGN_LOG=str(signing_log))
        self.run_build(env=environment)
        source = self.root / 'cargo/aarch64-apple-darwin/release/vmctl'
        self.assertEqual(source.stat().st_mtime_ns, timestamp + 1)
        self.assertEqual(installed.read_bytes(), source.read_bytes())
        self.assertIn('io.clankerd.vmctl', signing_log.read_text())
        before = self.artifact_state()
        signing_log.write_text('')
        self.run_build(env=environment)
        self.assertEqual(self.artifact_state(), before)
        self.assertEqual(signing_log.read_text(), '')

    def test_cargo_is_consulted_and_unchanged_executables_reuse_the_artifact(self):
        self.run_build()
        artifact = self.root / 'build/artifact'
        management = artifact / 'linux-arm64/management'
        self.assertEqual({p.name for p in (artifact / 'linux-arm64').iterdir()}, {'management'})
        self.assertTrue((management / 'usr/sbin/clankerd-guestd').is_file())
        self.assertFalse(any(p.is_file() or p.is_symlink() for p in management.iterdir()))
        original = self.artifact_state()
        self.run_build()
        self.assertEqual(self.artifact_state(), original)
        calls = self.log.read_text().splitlines()
        self.assertEqual(sorted(calls), ['guest', 'guest', 'host', 'host'])
        source = self.root / 'crates/libclankerd/src/unused.rs'
        source.write_text('fixture source addition')
        self.run_build()
        source.unlink()
        self.run_build()
        self.assertEqual(self.artifact_state(), original,
                         'Cargo outputs, rather than source inventory, determine repackaging')
        executable = self.root / 'cargo/aarch64-apple-darwin/release/vmctl'
        executable.unlink()
        self.run_build()
        self.assertTrue(executable.is_file())
        (management / 'sbin/dhcpcd').unlink()
        self.run_build()
        self.assertTrue((management / 'sbin/dhcpcd').is_file())

    def test_static_management_inputs_rebuild_the_filesystem_without_recompiling(self):
        self.run_build()
        artifact = self.root / 'build/artifact'
        config = self.root / 'crates/clankerd-guestd/rootfs/etc/clankerd/dhcpcd.conf'
        stamp = next((self.root / 'build/work').glob('*/management/.stamp'))
        previous = stamp.stat().st_mtime_ns
        config.write_text('duid ll\nnoipv4ll\n# fixture policy\n')
        os.utime(config, ns=(previous + 1, previous + 1))
        self.run_build()
        self.assertEqual((artifact / 'linux-arm64/management/etc/clankerd/dhcpcd.conf').read_text(),
                         'duid ll\nnoipv4ll\n# fixture policy\n')

    def test_build_rejects_an_existing_output_directory(self):
        output = self.root / 'build/artifact'
        output.mkdir(parents=True)
        (output / 'user-file').write_text('fixture user data')
        self.run_build(ok=False)
        self.assertFalse(output.is_symlink())
        self.assertEqual((output / 'user-file').read_text(), 'fixture user data')

    def test_publication_rejects_an_unrelated_symlink(self):
        output = self.root / 'build/artifact'
        output.parent.mkdir(exist_ok=True)
        unrelated = self.root / 'unrelated'
        unrelated.mkdir()
        (unrelated / 'user-file').write_text('fixture user data')
        output.symlink_to(unrelated)
        self.run_build(ok=False)
        self.assertEqual(output.resolve(strict=True), unrelated.resolve(strict=True))
        self.assertEqual((unrelated / 'user-file').read_text(), 'fixture user data')

    def test_publication_keeps_complete_bundles_and_reuses_the_warm_bundle(self):
        self.run_build()
        output = self.root / 'build/artifact'
        self.assertTrue(output.is_symlink())
        previous = output.resolve(strict=True)
        original = self.artifact_state()
        self.run_build()
        self.assertEqual(output.resolve(strict=True), previous)
        self.assertEqual(self.artifact_state(), original)
        self.run_build(env=dict(self.env, HOST_PAYLOAD='new host', GUEST_PAYLOAD='new guest'))
        self.assertNotEqual(output.resolve(strict=True), previous)
        self.assertTrue((output / 'darwin-arm64/vmctl').read_bytes().endswith(b'new host'))
        self.assertEqual((output / 'linux-arm64/management/usr/sbin/clankerd-guestd').read_bytes(),
                         b'compiled guestnew guest')
        for name, (mtime, content) in original.items():
            path = previous / name
            self.assertEqual(path.lstat().st_mtime_ns, mtime)
            self.assertEqual(os.readlink(path) if path.is_symlink() else path.read_bytes(), content)

    def test_publication_ignores_timestamps_and_publishes_tree_changes(self):
        import shlex
        import shutil
        self.run_build()
        output = self.root / 'build/artifact'
        previous = output.resolve(strict=True)
        original = self.artifact_state()
        staged = next((self.root / 'build/work').glob('*/artifact/darwin-arm64/vmctl'))
        timestamp = staged.stat().st_mtime_ns - 10_000_000_000
        os.utime(staged, ns=(timestamp, timestamp))
        self.run_build()
        self.assertEqual(output.resolve(strict=True), previous,
                         'Timestamp-only itemization must retain the published bundle')
        self.assertEqual(self.artifact_state(), original)
        self.run_build(env=dict(self.env, GUEST_PAYLOAD='new guest'))
        self.assertNotEqual(output.resolve(strict=True), previous)
        previous = output.resolve(strict=True)
        policy = self.root / 'crates/clankerd-guestd/rootfs/etc/group'
        policy.chmod(0o400)
        next((self.root / 'build/work').glob('*/management/.stamp')).unlink()
        self.run_build(env=dict(self.env, GUEST_PAYLOAD='new guest'))
        self.assertEqual((output / 'linux-arm64/management/etc/group').stat().st_mode & 0o777, 0o400)
        self.assertNotEqual(output.resolve(strict=True), previous)
        previous = output.resolve(strict=True)
        link = self.root / 'crates/clankerd-guestd/rootfs/etc/current-policy'
        link.symlink_to('passwd')
        self.run_build(env=dict(self.env, GUEST_PAYLOAD='new guest'))
        self.assertNotEqual(output.resolve(strict=True), previous)
        self.assertEqual((output / 'linux-arm64/management/etc/current-policy').readlink(), Path('passwd'))
        link.unlink()
        link.symlink_to('group')
        next((self.root / 'build/work').glob('*/management/.stamp')).unlink()
        self.run_build(env=dict(self.env, GUEST_PAYLOAD='new guest'))
        self.assertEqual((output / 'linux-arm64/management/etc/current-policy').readlink(), Path('group'))

    def test_tree_equivalence_covers_payload_entries(self):
        import shutil
        left = self.root / 'expected-tree'
        left.mkdir()
        (left / 'file').write_bytes(b'foo')
        (left / 'empty').mkdir()
        (left / 'link').symlink_to('file')
        for change in ('unchanged', 'timestamp', 'contents', 'permissions', 'link', 'added', 'removed', 'type', 'empty-directory'):
            with self.subTest(change=change):
                right = self.root / 'actual-tree'
                shutil.copytree(left, right, symlinks=True)
                if change == 'timestamp':
                    os.utime(right / 'file', ns=(1, 1))
                elif change == 'contents':
                    (right / 'file').write_bytes(b'bar')
                elif change == 'permissions':
                    (right / 'file').chmod(0o400)
                elif change == 'link':
                    (right / 'link').unlink()
                    (right / 'link').symlink_to('missing')
                elif change == 'added':
                    (right / 'extra').write_bytes(b'foo')
                elif change == 'removed':
                    (right / 'file').unlink()
                elif change == 'type':
                    (right / 'file').unlink()
                    (right / 'file').mkdir()
                elif change == 'empty-directory':
                    (right / 'empty').rmdir()
                result = subprocess.run(['python3', str(ROOT / 'cmake/scripts/compare-trees.py'), str(left), str(right)])
                self.assertEqual(result.returncode, 0 if change in ('unchanged', 'timestamp') else 1)
                shutil.rmtree(right)

    def test_publication_preserves_symlink_targets(self):
        import shlex
        import shutil
        link = self.root / 'crates/clankerd-guestd/rootfs/etc/current-policy'
        link.symlink_to('passwd')
        self.run_build()
        output = self.root / 'build/artifact'
        previous = output.resolve(strict=True)
        link.unlink()
        link.symlink_to('group')
        next((self.root / 'build/work').glob('*/management/.stamp')).unlink()
        self.run_build()
        self.assertEqual((output / 'linux-arm64/management/etc/current-policy').readlink(), Path('group'))
        self.assertNotEqual(output.resolve(strict=True), previous)
        previous = output.resolve(strict=True)
        original = self.artifact_state()
        self.run_build()
        self.assertEqual(output.resolve(strict=True), previous)
        self.assertEqual(self.artifact_state(), original)

        stage = next((self.root / 'build/work').glob('*/artifact'))
        staged = stage / 'linux-arm64/management/etc/current-policy'
        for target in ('missing-target', 'missing-target/', './missing-target', 'missing-target\n'):
            previous = output.resolve(strict=True)
            staged.unlink()
            staged.symlink_to(target)
            result = subprocess.run(['sh', str(self.root / 'cmake/scripts/publish.sh'),
                                     str(stage), str(output)], env=self.env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
            installed = output / 'linux-arm64/management/etc/current-policy'
            self.assertTrue(installed.is_symlink())
            self.assertEqual(os.readlink(installed), target)
            self.assertNotEqual(output.resolve(strict=True), previous)

    def test_failed_publication_preserves_the_bundle_and_cleans_the_candidate(self):
        import shlex
        import shutil
        self.run_build()
        output = self.root / 'build/artifact'
        previous = output.resolve(strict=True)
        original = self.artifact_state()
        bundles = set((self.root / 'build/artifact.bundles').iterdir())
        mover = self.root / 'tools/mv'
        mover.write_text('#!/bin/sh\nfor destination do :; done\n'
                         'if [ "${destination##*/}" = artifact ]; then exit 1; fi\n'
                         'exec ' + shlex.quote(shutil.which('mv')) + ' "$@"\n')
        mover.chmod(0o755)
        self.run_build(ok=False, env=dict(self.env, HOST_PAYLOAD='new host', GUEST_PAYLOAD='new guest'))
        self.assertEqual(output.resolve(strict=True), previous)
        self.assertEqual(self.artifact_state(), original)
        self.assertEqual(set((self.root / 'build/artifact.bundles').iterdir()), bundles)
        self.assertFalse(list((self.root / 'build').glob('artifact.bundle.*.link')))

    def test_failed_audit_does_not_publish_changed_host_or_guest_files(self):
        self.run_build()
        output = self.root / 'build/artifact'
        previous = output.resolve(strict=True)
        original = self.artifact_state()
        self.run_build(ok=False, env=dict(self.env, INVALID_HOST='1', GUEST_PAYLOAD='new guest'))
        staged = next((self.root / 'build/work').glob('*/artifact'))
        self.assertEqual((staged / 'darwin-arm64/vmctl').read_bytes(), b'invalid Mach-O')
        self.assertEqual((staged / 'linux-arm64/management/usr/sbin/clankerd-guestd').read_bytes(),
                         b'compiled guestnew guest')
        self.assertEqual(output.resolve(strict=True), previous)
        self.assertEqual(self.artifact_state(), original)
        self.run_build(env=dict(self.env, GUEST_PAYLOAD='new guest'))
        self.assertNotEqual(output.resolve(strict=True), previous)

    def test_interrupted_publication_preserves_the_bundle_and_cleans_the_candidate(self):
        import shlex
        import shutil
        import signal
        import time
        self.run_build()
        output = self.root / 'build/artifact'
        previous = output.resolve(strict=True)
        original = self.artifact_state()
        bundles = set((self.root / 'build/artifact.bundles').iterdir())
        marker = self.root / 'publishing'
        mover = self.root / 'tools/mv'
        mover.write_text('#!/bin/sh\nfor destination do :; done\n'
                         'if [ "${destination##*/}" = artifact ]; then '
                         'touch ' + shlex.quote(str(marker)) + '; sleep 30; fi\n'
                         'exec ' + shlex.quote(shutil.which('mv')) + ' "$@"\n')
        mover.chmod(0o755)
        process = subprocess.Popen(self.args, cwd=self.root,
            env=dict(self.env, HOST_PAYLOAD='new host', GUEST_PAYLOAD='new guest'),
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic() + 10
            while not marker.exists() and process.poll() is None and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists())
            self.assertEqual(output.resolve(strict=True), previous)
            self.assertEqual(self.artifact_state(), original)
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            process.communicate(timeout=10)
        self.assertNotEqual(process.returncode, 0)
        self.assertEqual(output.resolve(strict=True), previous)
        self.assertEqual(self.artifact_state(), original)
        deadline = time.monotonic() + 3
        while set((self.root / 'build/artifact.bundles').iterdir()) != bundles and time.monotonic() < deadline:
            time.sleep(.01)
        self.assertEqual(set((self.root / 'build/artifact.bundles').iterdir()), bundles)
        self.assertFalse(list((self.root / 'build').glob('artifact.bundle.*.link')))

    def test_failed_build_stages_preserve_the_published_artifact(self):
        self.run_build()
        artifact = self.root / 'build/artifact'
        original = self.artifact_state()
        self.run_build(ok=False, env=dict(self.env, FAIL_CARGO='1'))
        self.assertEqual(self.artifact_state(), original)
        self.run_build(ok=False, env=dict(self.env, HOST_PAYLOAD='changed host', FAIL_SIGN='1'))
        self.assertEqual(self.artifact_state(), original)
        self.assertTrue((artifact / 'darwin-arm64/vmctl').read_bytes().startswith(bytes.fromhex('cffaedfe')))
        archive = next((self.root / 'build/work/downloads').glob('base-*.archive'))
        stamp = next((self.root / 'build/work').glob('*/packages/base/.stamp'))
        previous = stamp.stat().st_mtime_ns
        archive.write_bytes(b'corrupt')
        os.utime(archive, ns=(previous + 1, previous + 1))
        self.run_build(ok=False)
        self.assertEqual(self.artifact_state(), original)

    def test_changed_cargo_output_and_locked_package_update_the_artifact(self):
        import gzip
        self.run_build()
        artifact = self.root / 'build/artifact'
        original = self.artifact_state()
        host = (artifact / 'darwin-arm64/vmctl').read_bytes()
        env = dict(self.env, GUEST_PAYLOAD='changed guest')
        self.run_build(env=env)
        self.assertNotEqual(self.artifact_state(), original)
        self.assertEqual((artifact / 'darwin-arm64/vmctl').read_bytes(), host)
        self.assertEqual((artifact / 'linux-arm64/management/usr/sbin/clankerd-guestd').read_bytes(),
                         b'compiled guestchanged guest')
        package = self.source / 'base.tar.gz'
        old = hashlib.sha256(package.read_bytes()).hexdigest()
        package.write_bytes(gzip.compress(gzip.decompress(package.read_bytes()), mtime=123))
        new = hashlib.sha256(package.read_bytes()).hexdigest()
        previous_lock = (artifact / 'inputs.cmake').stat().st_mtime_ns
        self.lock.write_text(self.lock.read_text().replace(old, new))
        os.utime(self.lock, ns=(previous_lock + 1, previous_lock + 1))
        previous = self.artifact_state()
        self.run_build(env=env)
        self.assertNotEqual(self.artifact_state(), previous)
        self.assertIn(new, (artifact / 'inputs.cmake').read_text())

    def test_interrupted_compilation_does_not_replace_installed_outputs(self):
        import signal
        import time
        self.run_build()
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
        self.run_build(env=dict(self.env, HOST_PAYLOAD="completed host"))
        self.assertNotEqual(self.artifact_state(), original)

    def test_an_incorrect_declared_hash_cannot_publish_an_archive_or_artifact(self):
        self.run_build()
        original = self.artifact_state()
        archive = self.source / 'base.tar.gz'
        old = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.lock.write_text(self.lock.read_text().replace(old, 'f'*64))
        self.run_build(ok=False)
        self.assertFalse(list((self.root / 'build/work/downloads').glob('base-'+'f'*64+'.archive')))
        self.assertEqual(self.artifact_state(), original)

if __name__ == '__main__':
    unittest.main()
