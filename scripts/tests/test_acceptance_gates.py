import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class AcceptanceConfiguration(unittest.TestCase):
    def test_relocated_checkout_reconfigures_without_removing_build_outputs(self):
        import json
        import sys
        cmake = shutil.which('cmake')
        self.assertIsNotNone(cmake, 'CMake must be installed for acceptance checks')
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            original = work / 'container-checkout'
            original.mkdir()
            dependency = work / 'dependency'
            dependency.mkdir()
            (dependency / 'CMakeLists.txt').write_text(
                'cmake_minimum_required(VERSION 3.25)\nproject(dependency NONE)\n'
                'file(WRITE "${CMAKE_BINARY_DIR}/dependency-source.txt" "${CMAKE_CURRENT_SOURCE_DIR}")\n')
            archive = shutil.make_archive(str(work / 'dependency'), 'gztar', dependency)
            fetch = ('project(acceptance NONE)\ninclude(FetchContent)\n'
                     f'FetchContent_Declare(dependency URL "{Path(archive).as_uri()}")\n'
                     'FetchContent_MakeAvailable(dependency)\n')
            (original / 'CMakeLists.txt').write_text(
                'cmake_minimum_required(VERSION 3.25)\n' + fetch)
            (original / 'CMakePresets.json').write_text(json.dumps({
                'version': 6, 'configurePresets': [{
                    'name': 'homebrew', 'generator': 'Ninja',
                    'binaryDir': '${sourceDir}/build/cmake',
                }],
            }))
            configured = subprocess.run([cmake, '--preset', 'homebrew'], cwd=original,
                                        capture_output=True, text=True)
            self.assertEqual(configured.returncode, 0, configured.stdout + configured.stderr)
            self.assertTrue((original / 'build/cmake/_deps/dependency-subbuild/CMakeCache.txt').is_file())
            retained = ['build/cmake/cargo/build/compiled', 'build/downloads/package',
                        'build/artifact/published']
            for name in retained:
                path = original / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(name)
            relocated = work / 'mac-checkout'
            shutil.copytree(original, relocated)
            shutil.rmtree(original)
            policy = (ROOT / 'CMakeLists.txt').read_text().split('project(', 1)[0]
            (relocated / 'CMakeLists.txt').write_text(policy + fetch)
            scripts = relocated / 'scripts'
            scripts.mkdir()
            for name in ['accept-native-build.sh', 'summarize-native-build.py']:
                shutil.copy2(ROOT / 'scripts' / name, scripts / name)
            tools = work / 'tools'
            tools.mkdir()
            commands = {
                'uname': '#!/bin/sh\necho Darwin arm64\n',
                'sw_vers': '#!/bin/sh\necho 15.8.1\n',
                'git': '#!/bin/sh\nexit 0\n',
                'cmake': f'#!{sys.executable}\nimport os,sys\n'
                         'if sys.argv[1:2] == ["--build"]: sys.exit(83)\n'
                         f'os.execv({cmake!r}, [{cmake!r}, *sys.argv[1:]])\n',
            }
            for name, content in commands.items():
                path = tools / name
                path.write_text(content)
                path.chmod(0o755)
            result = subprocess.run(['bash', str(scripts / 'accept-native-build.sh')],
                cwd=relocated, env=dict(os.environ, ACCEPTANCE_VERBOSE='1',
                    CLANKERD_CMAKE_PRESET='homebrew',
                    EVIDENCE_DIR=str(relocated / 'build/acceptance-evidence'),
                    PATH=str(tools)+os.pathsep+os.environ['PATH']),
                capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 83, result.stdout + result.stderr)
            cache = (relocated / 'build/cmake/CMakeCache.txt').read_text()
            self.assertIn(f'CMAKE_HOME_DIRECTORY:INTERNAL={relocated}', cache)
            self.assertEqual((relocated / 'build/cmake/dependency-source.txt').read_text(),
                             str(relocated / 'build/cmake/_deps/dependency-src'))
            for name in retained:
                self.assertEqual((relocated / name).read_text(), name)


class BootGate(unittest.TestCase):
    def test_workload_exit_leaves_management_running_until_explicit_stop(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            tools = work / 'tools'
            tools.mkdir()
            out = work / 'bundle'
            for name in ['darwin-arm64/clankerd-vmspawn', 'linux-arm64/management/usr/sbin/clankerd-guestd']:
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

    def test_gate_keeps_authorizing_session_but_has_its_own_process_group(self):
        import json
        import sys
        with tempfile.TemporaryDirectory() as directory:
            code = 'import json,os; print(json.dumps([os.getsid(0),os.getpgid(0),os.getpid()]))'
            result = self.run_gate(Path(directory) / 'gate.log', 'exec "$1" -c "$2"', sys.executable, code)
            self.assertEqual(result.returncode, 0, result.stderr)
            session, group, pid = json.loads(result.stdout)
            self.assertEqual(session, os.getsid(0))
            self.assertEqual(group, pid)
            self.assertNotEqual(group, os.getpgid(0))

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


class ScriptSnapshot(unittest.TestCase):
    def test_entrypoint_finishes_from_memory_after_its_file_is_replaced(self):
        prefix = (ROOT / 'scripts/accept-native-build.sh').read_text().split('root=', 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / 'accept.sh'
            # Padding ensures the continuation cannot rely on Bash's small input buffer.
            script.write_text(prefix + "printf 'ready\\n'\nread -r resume\n" +
                              '# padding\n' * 10000 + "printf 'snapshot-finished\\n'\n")
            with subprocess.Popen(['bash', str(script)], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as process:
                try:
                    self.assertEqual(process.stdout.readline(), 'ready\n')
                    script.write_text('"unterminated replacement\n')
                    stdout, stderr = process.communicate('\n', timeout=5)
                    self.assertEqual(process.returncode, 0, stderr)
                    self.assertEqual(stdout, 'snapshot-finished\n')
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.communicate()


class NetworkProcessLifecycle(unittest.TestCase):
    def test_only_the_recorded_pid_lifecycle_controls_completion(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('network_stopped', ROOT / 'scripts/check-network-stopped.py')
        checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(checker)
        with subprocess.Popen(['sleep', '10']) as target, subprocess.Popen(['sleep', '10']) as unrelated:
            try:
                with self.assertRaisesRegex(RuntimeError, 'outlived'):
                    checker.verify(target.pid, timeout=0)
                target.terminate()
                checker.verify(target.pid, timeout=2)
                target.wait()
                checker.verify(target.pid, timeout=0)
                self.assertIsNone(unrelated.poll())
            finally:
                target.terminate()
                target.wait()
                unrelated.terminate()
                unrelated.wait()

    def test_missing_or_system_pid_cannot_authorize_the_check(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('network_stopped', ROOT / 'scripts/check-network-stopped.py')
        checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(checker)
        for pid in [-1, 0, 1]:
            with self.subTest(pid=pid), self.assertRaises(ValueError):
                checker.verify(pid)
