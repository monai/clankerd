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
    def test_long_bundle_path_uses_recorded_pid_without_command_matching(self):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            bundle = evidence / ('long-acceptance-path-' * 8) / 'bundle'
            mappings = f'Path: {bundle}/darwin-arm64/clankerd-vmspawn\n{bundle}/darwin-arm64/lib/libkrun.1.dylib\n'
            with patch.object(runner.os, 'getuid', return_value=501), patch.object(
                    runner.subprocess, 'check_output', side_effect=[' 123 501\n', mappings]) as read:
                runner.verify(123, bundle, evidence)
                self.assertEqual(read.call_args_list[0].args[0],
                                 ['/bin/ps', '-p', '123', '-o', 'pid=,uid='])
                self.assertEqual((evidence / 'vmmap-123.log').read_text(), mappings)

    def test_root_owned_vm_is_rejected_before_reading_mappings(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(runner.os, 'getuid', return_value=501), patch.object(
                    runner.subprocess, 'check_output', return_value='123 0\n') as read:
                with self.assertRaises(AssertionError):
                    runner.verify(123, Path(directory), Path(directory))
                self.assertEqual(read.call_count, 1)

    def test_unbundled_library_is_rejected_and_evidence_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            with patch.object(runner.os, 'getuid', return_value=501), patch.object(
                    runner.subprocess, 'check_output', side_effect=['123 501\n', '/opt/homebrew/lib/libkrun.dylib']):
                with self.assertRaises(AssertionError):
                    runner.verify(123, evidence, evidence)
                self.assertTrue((evidence / 'vmmap-123.log').is_file())
