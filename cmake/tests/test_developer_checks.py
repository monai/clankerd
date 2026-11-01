"""Developer commands exercise Cargo without producing a distributable bundle."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class DeveloperChecks(unittest.TestCase):
    def setUp(self):
        import sys
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
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
        self.env = dict(os.environ, CHECK_LOG=str(self.log))
        self.project = f'''cmake_minimum_required(VERSION 3.28)
project(CheckFixture NONE)
set(CLANKERD_SOURCE_DIR "{self.root}")
set(CLANKERD_WORK_DIR "{self.root}/work")
set(CLANKERD_CARGO_TARGET_DIR "{self.root}/cargo-output")
set(CLANKERD_CARGO_EXECUTABLE "{cargo}")
set(CLANKERD_ARTIFACT_DIR "{self.root}/selected bundle")
set(Python3_EXECUTABLE "{sys.executable}")
add_custom_target(build COMMAND "${{CMAKE_COMMAND}}" -E make_directory "${{CLANKERD_ARTIFACT_DIR}}")
include("{ROOT}/cmake/Checks.cmake")
'''
        (self.root / 'CMakeLists.txt').write_text(self.project)

    def run_target(self, target, system):
        configured = subprocess.run(['cmake', '-S', str(self.root), '-B', str(self.root / 'configured'),
            '-G', 'Ninja', f'-DCLANKERD_CHECK_SYSTEM={system}'], env=self.env,
            text=True, capture_output=True)
        self.assertEqual(configured.returncode, 0, configured.stdout + configured.stderr)
        return subprocess.run(['cmake', '--build', str(self.root / 'configured'), '--target', target],
            env=self.env, text=True, capture_output=True)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_ordinary_rust_checks_do_not_build_the_product_artifact(self):
        for system in ['Linux', 'Darwin']:
            for target in ['test-rust', 'lint']:
                with self.subTest(system=system, target=target):
                    result = self.run_target(target, system)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertFalse((self.root / 'selected bundle').exists())
        self.assertTrue(all(call['args'][0] in {'test', 'fmt', 'clippy'} for call in self.calls()))

    def test_linux_suite_and_mac_selection_match_the_execution_host(self):
        self.env['RUSTFLAGS'] = '-C debuginfo=1'
        result = self.run_target('test-rust', 'Linux')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.calls()[-1]['args'], ['test', '--locked', '--workspace'])
        self.log.unlink()
        result = self.run_target('test-rust', 'Darwin')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls()
        self.assertIn('--exclude', calls[0]['args'])
        self.assertIn('clankerd-guestd', calls[0]['args'])
        self.assertFalse(any('--ignored' in call['args'] for call in calls))
        self.assertTrue(all(call['rustflags'] == '-C debuginfo=1' for call in calls))

    def test_wrong_host_native_and_guest_checks_fail_without_launching_cargo(self):
        for target, system in [('test-native', 'Linux'), ('test-linux', 'Darwin')] + [(target, 'FreeBSD') for target in ['test-native', 'test-linux', 'test-rust', 'lint']]:
            result = self.run_target(target, system)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(self.log.exists())
            self.assertFalse((self.root / 'selected bundle').exists())

    def test_cargo_failure_fails_the_developer_target(self):
        self.env['FAIL_CARGO'] = 'clippy'
        result = self.run_target('lint', 'Linux')
        self.assertNotEqual(result.returncode, 0)

    def test_native_checks_build_and_use_the_selected_bundle(self):
        result = self.run_target('test-native', 'Darwin')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.root / 'selected bundle').is_dir())
        self.assertEqual(self.calls()[-1]['out'], str(self.root / 'selected bundle'))
        self.assertIn('--ignored', self.calls()[-1]['args'])
