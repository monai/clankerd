import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BootGate(unittest.TestCase):
    def test_workload_exit_leaves_management_running_until_explicit_stop(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            tools = work / 'tools'
            tools.mkdir()
            out = work / 'bundle'
            for name in ['darwin-arm64/lib/libkrun.1.dylib', 'darwin-arm64/lib/libkrunfw.5.dylib',
                         'darwin-arm64/clankerd-vmspawn', 'linux-arm64/clankerd-guestd']:
                path = out / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
                path.chmod(0o755)
            commands = {
                tools / 'uname': '#!/bin/sh\necho Darwin arm64\n',
                tools / 'codesign': '#!/bin/sh\necho com.apple.security.hypervisor\n',
                out / 'darwin-arm64/vmctl': '''#!/bin/sh
case "$1" in
 run) touch "$BOOT_RUNNING"; exit 2;;
 logs) echo 'clankerd-guestd: unknown argument --nope';;
 stop) rm -f "$BOOT_RUNNING";;
 rm) if [ "$2" = boot-check ] && [ -f "$BOOT_RUNNING" ]; then echo 'cannot remove running machine' >&2; exit 1; fi;;
 *) exit 1;;
esac
''',
            }
            for path, content in commands.items():
                path.write_text(content)
                path.chmod(0o755)
            result = subprocess.run(['bash', str(ROOT / 'scripts/m2-boot-check.sh'), 'boot'],
                                    env=dict(os.environ, OUT=str(out), PATH=str(tools)+os.pathsep+os.environ['PATH'],
                                             BOOT_RUNNING=str(work / 'running')),
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse((work / 'running').exists())


class GateRunner(unittest.TestCase):
    def run_gate(self, log, script, *args, timeout='5'):
        import sys
        return subprocess.run(
            [sys.executable, str(ROOT / 'scripts/run-acceptance-gate.py'), str(log),
             'bash', '-c', script, 'gate', *map(str, args)],
            env=dict(os.environ, ACCEPTANCE_GATE_TIMEOUT=timeout),
            capture_output=True, text=True, timeout=10)

    def test_streams_evidence_and_preserves_gate_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / 'gate.log'
            result = self.run_gate(log, 'echo phase-one; echo phase-two >&2; exit 7')
            self.assertEqual(result.returncode, 7, result.stdout + result.stderr)
            self.assertIn('phase-one\nphase-two\n', result.stdout)
            self.assertEqual(log.read_text(), 'phase-one\nphase-two\n')

    def test_timeout_runs_shell_cleanup_and_retains_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            result = self.run_gate(work / 'gate.log',
                '''trap 'echo cleanup; touch "$1"; exit 0' TERM
 echo waiting
 while :; do sleep 1; done''', work / 'cleaned', timeout='.3')
            self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
            self.assertIn('TIMEOUT', result.stdout)
            self.assertTrue((work / 'cleaned').is_file(), result.stdout)
            self.assertIn('cleanup', (work / 'gate.log').read_text())
