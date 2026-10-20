"""Developer checks run independently of artifact production."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).parents[2]


class DeveloperChecks(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / 'build-support').mkdir()
        shutil.copy2(ROOT / 'build-support/checks.mk', self.root / 'build-support/checks.mk')
        (self.root / 'Makefile').write_text('WORK := work\ninclude build-support/checks.mk\n')
        self.log = self.root / 'cargo.jsonl'
        cargo = self.root / 'cargo'
        cargo.write_text('''#!/usr/bin/env python3
import json, os, sys
with open(os.environ['CHECK_LOG'], 'a') as log:
    log.write(json.dumps({'args': sys.argv[1:], 'out': os.environ.get('OUT'),
                         'rustflags': os.environ.get('RUSTFLAGS', '')}) + '\\n')
if os.environ.get('FAIL_CARGO') == sys.argv[1]:
    sys.exit(1)
''')
        cargo.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ['PATH'],
                        CHECK_LOG=str(self.log))

    def run_make(self, target, system, **variables):
        return subprocess.run(['make', '--no-print-directory', target,
                               'BUILD_CONTEXT=Linux-x86_64', f'CHECK_SYSTEM={system}',
                               *(f'{name}={value}' for name, value in variables.items())],
                              cwd=self.root, env=self.env, text=True, capture_output=True)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_ordinary_rust_checks_do_not_build_the_product_artifact(self):
        for system in ['Linux', 'Darwin']:
            for target in ['test-rust', 'lint']:
                with self.subTest(system=system, target=target):
                    result = self.run_make(target, system)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertFalse((self.root / 'build').exists())
        self.assertTrue(all(call['args'][0] in {'test', 'fmt', 'clippy'} for call in self.calls()))

    def test_linux_suite_and_mac_selection_match_the_execution_host(self):
        self.env['RUSTFLAGS'] = '-C debuginfo=1'
        result = self.run_make('test-rust', 'Linux')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls()[-1]['args'], ['test', '--locked', '--workspace'])
        self.log.unlink()
        result = self.run_make('test-rust', 'Darwin')
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertIn('--exclude', calls[0]['args'])
        self.assertIn('clankerd-guestd', calls[0]['args'])
        self.assertFalse(any('--ignored' in call['args'] for call in calls))
        self.assertTrue(all(call['rustflags'] == '-C debuginfo=1' for call in calls))

    def test_wrong_host_native_and_guest_checks_fail_without_launching_cargo(self):
        for target, system in [('test-native', 'Linux'), ('test-linux', 'Darwin')]:
            result = self.run_make(target, system)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(self.log.exists())
            self.assertFalse((self.root / 'build').exists())

    def test_cargo_failure_fails_the_developer_target(self):
        self.env['FAIL_CARGO'] = 'clippy'
        result = self.run_make('lint', 'Linux')
        self.assertNotEqual(result.returncode, 0)

    def test_native_checks_build_and_use_the_selected_bundle(self):
        # Supply the artifact producer at its Make boundary; its own publication
        # and signing contract is covered by test_build_graph.py.
        (self.root / 'Makefile').write_text('include build-support/checks.mk\n'
            'build:\n\tmkdir -p "$(RUST_OUT)"\n\ttouch "$(RUST_OUT)/completed"\n')
        bundle = self.root / 'selected bundle'
        result = self.run_make('test-native', 'Darwin', OUT=bundle)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((bundle / 'completed').is_file())
        self.assertEqual(self.calls()[-1]['out'], str(bundle))
        self.assertIn('--ignored', self.calls()[-1]['args'])
