import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class NetworkHelper(unittest.TestCase):
    def run_installer(self, *, registered=True, stop_polls=3, bootout_error=0):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            state = directory / 'state.json'
            calls = directory / 'calls.jsonl'
            state.write_text(json.dumps(dict(registered=registered, stop_polls=stop_polls,
                                            stopping=False, bootout_error=bootout_error)))
            tool = directory / 'tool'
            tool.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
state_path = pathlib.Path(os.environ['HELPER_TEST_STATE'])
calls_path = pathlib.Path(os.environ['HELPER_TEST_CALLS'])
state = json.loads(state_path.read_text())
name = pathlib.Path(sys.argv[0]).name
with calls_path.open('a') as output:
    output.write(json.dumps([name, *sys.argv[1:]]) + '\\n')
result = 0
if name == 'launchctl':
    action = sys.argv[1]
    if action == 'print':
        if state['stopping']:
            if state['stop_polls'] == 0:
                state['registered'] = False
            elif state['stop_polls'] > 0:
                state['stop_polls'] -= 1
        result = 0 if state['registered'] else 113
    elif action == 'bootout':
        result = state['bootout_error']
        if not result:
            state['stopping'] = True
    elif action == 'bootstrap':
        result = 5 if state['registered'] else 0
state_path.write_text(json.dumps(state))
sys.exit(result)
''')
            tool.chmod(0o755)
            script = (ROOT / 'cmake/scripts/network-helper.sh').read_text()
            for executable in ['/bin/launchctl', '/usr/bin/plutil', '/usr/bin/install',
                               '/bin/mv', '/bin/rm', '/bin/sleep']:
                mock = directory / Path(executable).name
                mock.symlink_to(tool)
                script = script.replace(executable, str(mock))
            installer = directory / 'install.sh'
            installer.write_text(script)
            result = subprocess.run(['sh', str(installer), 'install', '/build/helper', '/build/helper.plist'],
                                    env={**os.environ, 'HELPER_TEST_STATE': str(state),
                                         'HELPER_TEST_CALLS': str(calls)},
                                    capture_output=True, text=True, timeout=10)
            return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def test_reinstall_waits_for_asynchronous_removal_before_bootstrap(self):
        result, calls = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        actions = [call[1] for call in calls if call[0] == 'launchctl']
        self.assertEqual(actions[-1], 'bootstrap')

    def test_first_install_succeeds_without_an_existing_registration(self):
        result, calls = self.run_installer(registered=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls[-1][1], 'bootstrap')

    def test_bootout_failure_is_not_hidden(self):
        result, calls = self.run_installer(bootout_error=7)
        self.assertEqual(result.returncode, 7)
        self.assertFalse(any(call[0] == 'launchctl' and call[1] == 'bootstrap' for call in calls))

    def test_removal_timeout_does_not_attempt_bootstrap(self):
        result, calls = self.run_installer(stop_polls=-1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('still registered', result.stderr)
        self.assertFalse(any(call[0] == 'launchctl' and call[1] == 'bootstrap' for call in calls))


if __name__ == '__main__':
    unittest.main()
