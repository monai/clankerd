import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    'acceptance_summary', Path(__file__).parents[1] / 'summarize-self-contained-build.py')
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class AcceptanceSummary(unittest.TestCase):
    def test_failure_before_verbose_cleanup_is_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            (evidence / 'result.json').write_text(json.dumps({
                'status': 'FAIL', 'exit_code': 1,
                'gates': ['FAIL\tm2-boot-check', 'PASS\tm2-smoke-test'],
                'management': {'status': 'PASS'}}))
            text = ('== image user\nworkload-user=agent workload-pwd=/workspace\n'
                    'FAIL: no cgroup v2 controllers delegated\n'
                    '== /tmp/clankerd-boot-check.fixture/state/machines/id/vmspawn.log\n' +
                    'WARN: vsock ENOTCONN\n' * 1000)
            (evidence / 'm2-boot-check.log').write_text(text)
            report = runner.summarize(evidence)
            self.assertIn('FAIL: no cgroup v2 controllers delegated', report)
            self.assertIn('PASS\tm2-smoke-test', report)
            self.assertNotIn('vsock', report)
            self.assertLess(len(report.splitlines()), 30)
            self.assertEqual((evidence / 'm2-boot-check.log').read_text(), text)

    def test_pre_gate_failure_is_summarized_and_long_lines_are_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            (evidence / 'result.json').write_text(json.dumps({
                'status': 'FAIL', 'exit_code': 1, 'gates': []}))
            (evidence / 'acceptance.log').write_text('compile\nerror: ' + 'x' * 10000)
            report = runner.summarize(evidence)
            self.assertIn('error:', report)
            self.assertIn('[truncated; see log]', report)
            self.assertLess(len(report), 1000)

    def test_live_progress_hides_verbose_output_but_keeps_numbered_steps(self):
        import subprocess
        import sys
        result = subprocess.run([sys.executable, str(Path(runner.__file__)), '--progress'],
            input='[2/10] Build (long-running)\nnoisy compiler output\nPASS\tm2-smoke-test\nWARN vsock\n',
            text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '[2/10] Build (long-running)\nPASS\tm2-smoke-test\n')
