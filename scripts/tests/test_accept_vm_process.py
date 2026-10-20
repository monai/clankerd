"""Acceptance evidence must prove an unprivileged VMM and installed dependencies."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    'accept_vm_process', Path(__file__).parents[1] / 'accept-vm-process.py')
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class VmProcessEvidence(unittest.TestCase):
    def test_only_the_requesting_nonroot_identity_is_accepted(self):
        for owner in (0, 500, 501):
            with self.subTest(owner=owner), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                mappings = f'Path: {root}/darwin-arm64/clankerd-vmspawn\n/system-libraries/libkrun.1.dylib\n'
                with patch.object(runner.os, 'getuid', return_value=501), patch.object(
                        runner.subprocess, 'check_output', side_effect=[f'123 {owner}\n', mappings]):
                    if owner == 501:
                        runner.verify(123, root, root)
                        self.assertEqual((root / 'vmmap-123.log').read_text(), mappings)
                    else:
                        with self.assertRaises(AssertionError):
                            runner.verify(123, root, root)
                    self.assertEqual((root / 'process-123.log').read_text(), f'123 {owner}\n')

    def test_mapping_evidence_requires_the_executable_and_libkrun(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for mappings in ['', '/system-libraries/libkrun.dylib',
                             f'{root}/darwin-arm64/clankerd-vmspawn', f'/system-libraries/libkrun.1.dylib']:
                with self.subTest(mappings=mappings), patch.object(runner.os, 'getuid', return_value=501), patch.object(
                        runner.subprocess, 'check_output', side_effect=['123 501\n', mappings]):
                    with self.assertRaises(AssertionError):
                        runner.verify(123, root, root)
                    self.assertEqual((root / 'vmmap-123.log').read_text(), mappings)
