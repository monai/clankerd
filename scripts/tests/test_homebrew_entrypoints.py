"""Exercise native library configuration at its external command boundary."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class HomebrewEntrypoints(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.log = self.root / 'command.json'
        self.tool('uname', 'import sys; print("Darwin arm64" if sys.argv[1:] == ["-sm"] else "Darwin")')
        self.tool('brew', '''import os,sys
args=sys.argv[1:]
if args == ['deps','--installed','--formula','libkrun']:
 print('libkrunfw\\nxz\\nrenderer')
elif args[:1] == ['--prefix'] and len(args) == 2:
 print(os.environ['BREW_TEST_PREFIX']+'/'+args[1])
else:
 sys.exit('unexpected brew command: '+repr(args))
''')
        self.tool('record-command', '''import json,os,sys
from pathlib import Path
Path(os.environ['ENTRYPOINT_LOG']).write_text(json.dumps({'args':sys.argv[1:],'env':dict(os.environ)}))
sys.exit(int(os.environ.get('COMMAND_EXIT','0')))
''')
        self.env = dict(os.environ, PATH=str(self.tools)+os.pathsep+os.environ['PATH'],
                        ENTRYPOINT_LOG=str(self.log), BREW_TEST_PREFIX=str(self.root / 'installed libraries'))
        for key in ['HOST_RUSTFLAGS', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS']:
            self.env.pop(key, None)

    def tool(self, name, code):
        import sys
        path = self.tools / name
        path.write_text(f'#!{sys.executable}\n'+code+'\n')
        path.chmod(0o755)

    def run_environment(self, homebrew=True, system='Darwin'):
        import sys
        script = self.root / 'environment.cmake'
        script.write_text(f'''set(CMAKE_HOST_SYSTEM_NAME "{system}")
set(CMAKE_HOST_SYSTEM_PROCESSOR "arm64")
set(CLANKERD_HOMEBREW {'ON' if homebrew else 'OFF'})
include("{ROOT}/cmake/HostEnvironment.cmake")
execute_process(COMMAND "${{CMAKE_COMMAND}}" -E env ${{CLANKERD_HOST_ENV}}
    "{sys.executable}" "{self.tools}/record-command" RESULT_VARIABLE status)
if(NOT status EQUAL 0)
    message(FATAL_ERROR "Configured command failed with exit code ${{status}}")
endif()
''')
        return subprocess.run(['cmake', '-P', str(script)], cwd=ROOT,
            env=self.env, capture_output=True, text=True)

    def test_homebrew_configures_host_linking_and_runtime_without_dyld(self):
        self.env['DYLD_LIBRARY_PATH'] = '/stale/override'
        self.env['RUSTFLAGS'] = '-C debuginfo=1'
        result = self.run_environment()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        environment = json.loads(self.log.read_text())['env']
        self.assertFalse('DYLD_LIBRARY_PATH' in environment, 'Host command must discard stale dyld overrides')
        flags = environment['CARGO_ENCODED_RUSTFLAGS'].split('\x1f')
        self.assertEqual(flags[:2], ['-C', 'debuginfo=1'])
        for formula in ['libkrun', 'libkrunfw', 'xz', 'renderer']:
            library = str(self.root / 'installed libraries' / formula / 'lib')
            self.assertIn('-Lnative=' + library, flags)
            self.assertIn('-Clink-arg=' + library, flags)
        self.assertIn('-Clink-arg=-rpath', flags)

    def test_encoded_host_flags_override_plain_rustflags(self):
        self.env['RUSTFLAGS'] = '-C debuginfo=1'
        self.env['CARGO_ENCODED_RUSTFLAGS'] = '-C\x1fopt-level=1'
        self.env['HOST_RUSTFLAGS'] = '-Lnative=/chosen path\x1f-Copt-level=2'
        result = self.run_environment()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        flags = json.loads(self.log.read_text())['env']['CARGO_ENCODED_RUSTFLAGS'].split('\x1f')
        self.assertEqual(flags[:2], ['-Lnative=/chosen path', '-Copt-level=2'])

    def test_formula_aliases_do_not_duplicate_library_search_paths_or_rpaths(self):
        self.tool('brew', '''import os,sys
if sys.argv[1:] == ['deps','--installed','--formula','libkrun']:
 print('krun/libkrunfw\\nxz')
elif sys.argv[1:2] == ['--prefix']:
 print(os.environ['BREW_TEST_PREFIX']+'/'+sys.argv[2].rsplit('/', 1)[-1])
else:
 sys.exit(1)
''')
        result = self.run_environment()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        flags = json.loads(self.log.read_text())['env']['CARGO_ENCODED_RUSTFLAGS'].split('\x1f')
        library = str(self.root / 'installed libraries/libkrunfw/lib')
        self.assertEqual(flags.count('-Lnative=' + library), 1)
        self.assertEqual(flags.count('-Clink-arg=' + library), 1)

    def test_command_failure_is_propagated(self):
        self.env['COMMAND_EXIT'] = '7'
        self.assertNotEqual(self.run_environment().returncode, 0)

    def test_unsupported_host_fails_before_homebrew_or_build(self):
        self.tool('brew', 'raise RuntimeError("must not invoke brew")')
        result = self.run_environment(system='Linux')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Apple Silicon macOS', result.stderr)
        self.assertFalse(self.log.exists())

    def test_default_configuration_does_not_invoke_homebrew(self):
        self.tool('brew', 'raise RuntimeError("must not invoke brew")')
        self.env['HOST_RUSTFLAGS'] = '-Lnative=/installed library\x1f-Clink-arg=-rpath'
        result = self.run_environment(homebrew=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        environment = json.loads(self.log.read_text())['env']
        self.assertEqual(environment['CARGO_ENCODED_RUSTFLAGS'], self.env['HOST_RUSTFLAGS'])
