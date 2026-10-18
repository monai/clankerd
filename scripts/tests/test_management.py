"""Packaged guest filesystem contract: paths, provisioning, and architecture."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).parents[1] / 'assemble-management.py'


class ManagementFilesystem(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.package = self.root / 'package'
        for name, content in {
            'etc/passwd': b'root:x:0:0:root:/root:/bin/sh\nfixture:x:100:100::/:/bin/sh\n',
            'etc/group': b'root:x:0:\nfixture:x:101:\n',
            'bin/sh': b'fixture shell', 'sbin/nologin': b'fixture nologin',
            'sbin/dhcpcd': b'fixture DHCP', 'sbin/mke2fs': b'fixture formatter',
            'usr/sbin/resize2fs': b'fixture grower', 'lib/library.so': b'fixture library',
        }.items():
            path = self.package / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
            path.chmod(0o755 if name.startswith(('bin/', 'sbin/', 'usr/sbin/')) else 0o644)
        (self.package / 'lib/alias.so').symlink_to('library.so')

    def assemble(self, required=('sbin/mke2fs', 'usr/sbin/resize2fs')):
        return subprocess.run([sys.executable, str(SCRIPT), str(self.root / 'output'),
                               str(self.package), '--required', *required],
                              text=True, capture_output=True)

    def test_package_paths_contents_links_and_permissions_are_preserved(self):
        result = self.assemble()
        self.assertEqual(result.returncode, 0, result.stderr)
        out = self.root / 'output'
        for name in ['sbin/mke2fs', 'sbin/dhcpcd', 'usr/sbin/resize2fs', 'lib/library.so']:
            self.assertEqual((out / name).read_bytes(), (self.package / name).read_bytes())
            self.assertEqual((out / name).stat().st_mode & 0o777, (self.package / name).stat().st_mode & 0o777)
        self.assertEqual(os.readlink(out / 'lib/alias.so'), 'library.so')
        self.assertEqual({p.name for p in out.iterdir()}, {'etc', 'bin', 'sbin', 'usr', 'lib', 'var', 'run'})

    def test_account_and_hook_provisioning_uses_available_identity_and_explicit_mode(self):
        result = self.assemble()
        self.assertEqual(result.returncode, 0, result.stderr)
        out = self.root / 'output'
        account = next(line.split(':') for line in (out / 'etc/passwd').read_text().splitlines()
                       if line.startswith('dhcpcd:'))
        self.assertNotIn(account[2], {'0', '100', '101'})
        self.assertEqual(account[2], account[3])
        self.assertIn(f'dhcpcd:x:{account[2]}:', (out / 'etc/group').read_text())
        self.assertTrue((out / 'var/lib/dhcpcd').is_dir())
        hook = out / 'usr/lib/clankerd/network-hook'
        self.assertEqual(hook.read_text(), '#!/bin/sh\nexec /usr/sbin/clankerd-guestd network-hook\n')
        self.assertTrue(hook.stat().st_mode & 0o111)

    def test_required_programs_reject_missing_files_and_foreign_architectures(self):
        for content in [None, b'\x7fELF' + bytes([2]) + bytes(13) + bytes([62, 0])]:
            with self.subTest(content=content):
                target = self.package / 'sbin/mke2fs'
                if content is None:
                    target.unlink()
                else:
                    target.write_bytes(content)
                result = self.assemble()
                self.assertNotEqual(result.returncode, 0)
                import shutil
                shutil.rmtree(self.root / 'output')

    def test_required_programs_must_be_executable(self):
        (self.package / 'sbin/mke2fs').chmod(0o644)
        result = self.assemble()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not executable', result.stderr)
