"""Verified archives remain guest filesystems; no entry or install script runs on the host."""
import io
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).parents[1] / 'extract-binary.py'


class BinaryExtraction(unittest.TestCase):
    def extract(self, root, entries):
        archive = root / 'package.tar'
        with tarfile.open(archive, 'w') as output:
            for entry, content in entries:
                entry.size = len(content)
                output.addfile(entry, io.BytesIO(content))
        return subprocess.run([sys.executable, str(SCRIPT), str(archive), str(root / 'out')],
                              capture_output=True, text=True)

    def test_payload_modes_and_guest_absolute_links_remain_inside_the_guest_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            program = tarfile.TarInfo('usr/sbin/example')
            program.mode = 0o755
            link = tarfile.TarInfo('sbin/example')
            link.type = tarfile.SYMTYPE
            link.linkname = '/usr/sbin/example'
            install = tarfile.TarInfo('.post-install')
            marker = root / 'executed'
            result = self.extract(root, [(program, b'fixture program'), (link, b''),
                (install, f'#!/bin/sh\ntouch {marker}\n'.encode())])
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((root / 'out/sbin/example').resolve(), root / 'out/usr/sbin/example')
            self.assertEqual((root / 'out/usr/sbin/example').read_bytes(), b'fixture program')
            self.assertEqual((root / 'out/usr/sbin/example').stat().st_mode & 0o777, 0o755)
            self.assertTrue((root / 'out/package-metadata/.post-install').is_file())
            self.assertFalse(marker.exists())

    def test_paths_and_links_cannot_escape_the_destination(self):
        for name, link in [('../outside', None), ('etc/../../outside', None),
                           ('/outside', None), ('usr/link', '../../../outside')]:
            with self.subTest(name=name, link=link), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                entry = tarfile.TarInfo(name)
                if link:
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = link
                result = self.extract(root, [(entry, b'')])
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((root / 'outside').exists())
