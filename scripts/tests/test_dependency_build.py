"""Exercise dependency cache publication without compilers or network access."""
import concurrent.futures
import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('dependency_build', Path(__file__).resolve().parents[1] / 'dependency-build.py')
build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build)


class DependencyCacheTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='dependency build test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.cache = self.root / 'cache'
        self.cache.mkdir()

    def test_concurrent_download_and_corruption_recovery(self):
        source = self.root / 'source'
        source.write_bytes(b'pinned source')
        checksum = hashlib.sha256(source.read_bytes()).hexdigest()
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            paths = list(pool.map(lambda _: build.download(self.cache, checksum, source.as_uri()), range(8)))
        self.assertEqual(len(set(paths)), 1)
        self.assertEqual(paths[0].read_bytes(), source.read_bytes())
        paths[0].write_bytes(b'corrupt')
        self.assertEqual(build.download(self.cache, checksum, source.as_uri()).read_bytes(), source.read_bytes())
        with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
            build.download(self.cache, '0' * 64, source.as_uri())
        self.assertFalse((self.cache / ('0' * 64 + '.tar')).exists())
        self.assertFalse(list(self.cache.glob('download-*')))

    def test_completed_build_cache_and_failed_build_retry(self):
        source = self.root / 'source'
        source.mkdir()
        (source / 'configure').write_text('fixture')
        archive = self.root / 'source.tar'
        with tarfile.open(archive, 'w') as output:
            output.add(source, arcname='source')
        checksum = build.digest(archive)
        calls = []
        fail = False

        def run(args, **kwargs):
            calls.append(args)
            if args[0] == 'tar':
                subprocess.run(args, check=True)
            if args[0] == './configure':
                env = kwargs['env']
                self.assertEqual(env['CC'], 'clankerd-cc')
                self.assertEqual(env['BUILD_CC'], 'cc')
                launcher = Path(env['PATH'].split(os.pathsep)[0]) / env['CC']
                self.assertTrue(launcher.is_file())
            if args[0] == 'make':
                if fail:
                    raise subprocess.CalledProcessError(1, args)
                for artifact in build.ARTIFACTS['e2fsprogs']:
                    path = kwargs['cwd'] / artifact
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b'compiled fixture')

        with patch.dict(os.environ, {'E2FSPROGS_CACHE': str(self.cache), 'BUILD_JOBS': '2'}), \
             patch.dict(build.PACKAGES, {'e2fsprogs': ('fixture', checksum, archive.as_uri())}), \
             patch.object(build, 'run', side_effect=run), \
             patch.object(build.subprocess, 'check_output', return_value='compiler fixture'), \
             patch.object(build.shutil, 'which', return_value='/usr/bin/cc'), \
             contextlib.redirect_stdout(io.StringIO()):
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                list(pool.map(lambda output: build.build('e2fsprogs', self.root / output), ['out', 'concurrent-out']))
            self.assertEqual(sum(args[0] == 'make' for args in calls), 1)
            first_count = len(calls)
            build.build('e2fsprogs', self.root / 'second-out')
            self.assertEqual(len(calls), first_count)
            self.assertTrue(any(args[:2] == ['make', '-j2'] for args in calls))
            with patch.dict(os.environ, {'CFLAGS': '-O3'}):
                fail = True
                with self.assertRaises(subprocess.CalledProcessError):
                    build.build('e2fsprogs', self.root / 'failed-out')
                self.assertFalse((self.root / 'failed-out').exists())
                self.assertEqual(len(list(self.cache.glob('artifacts-*'))), 1)
                fail = False
                build.build('e2fsprogs', self.root / 'new-flags-out')
            self.assertEqual(len(list(self.cache.glob('artifacts-*'))), 2)
            self.assertFalse(list(self.cache.glob('work-*')))
            self.assertEqual(len(list((self.root / 'out').iterdir())), 4)


if __name__ == '__main__':
    unittest.main()
