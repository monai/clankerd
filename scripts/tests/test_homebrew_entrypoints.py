"""Exercise the Make entrypoints at their external tool boundaries."""
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
        self.tool('record-make', '''import json,os,sys
from pathlib import Path
Path(os.environ['ENTRYPOINT_LOG']).write_text(json.dumps({'args':sys.argv[1:],'env':dict(os.environ)}))
sys.exit(int(os.environ.get('MAKE_EXIT','0')))
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

    def run_make(self, target, **variables):
        return subprocess.run(['make', '--no-print-directory', target,
            f'MAKE={self.tools / "record-make"}', *(f'{key}={value}' for key,value in variables.items())],
            cwd=ROOT, env=self.env, capture_output=True, text=True)

    def test_homebrew_build_configures_host_linking_and_runtime_without_dyld(self):
        self.env['DYLD_LIBRARY_PATH'] = '/stale/override'
        self.env['RUSTFLAGS'] = '-C debuginfo=1'
        result = self.run_make('build-homebrew')
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        recorded = json.loads(self.log.read_text())
        self.assertEqual(recorded['args'], ['build'])
        environment = recorded['env']
        self.assertNotIn('DYLD_LIBRARY_PATH', environment)
        self.assertEqual(environment['RUSTFLAGS'], '-C debuginfo=1')
        self.assertNotIn('CARGO_ENCODED_RUSTFLAGS', environment)
        flags = environment['HOST_RUSTFLAGS'].split('\x1f')
        self.assertEqual(flags[:2], ['-C', 'debuginfo=1'])
        for formula in ['libkrun', 'libkrunfw', 'xz', 'renderer']:
            library = str(self.root / 'installed libraries' / formula / 'lib')
            self.assertIn('-Lnative='+library, flags)
            self.assertIn('-Clink-arg='+library, flags)
        self.assertIn('-Clink-arg=-rpath', flags)

    def test_homebrew_acceptance_reuses_the_core_acceptance_target(self):
        result = self.run_make('test-acceptance-homebrew', ACCEPTANCE_GATES='m2-smoke-lifetimes')
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        self.assertEqual(json.loads(self.log.read_text())['args'], ['test-acceptance'])

    def test_failure_is_propagated(self):
        self.env['MAKE_EXIT'] = '7'
        result = self.run_make('build-homebrew')
        self.assertNotEqual(result.returncode, 0)

    def test_unsupported_host_fails_before_homebrew_or_build(self):
        self.tool('uname', 'import sys; print("Linux aarch64" if sys.argv[1:] == ["-sm"] else "Linux")')
        self.tool('brew', 'raise RuntimeError("must not invoke brew")')
        result = self.run_make('build-homebrew')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Apple Silicon macOS', result.stderr)
        self.assertFalse(self.log.exists())

    def test_adapter_flags_reach_host_cargo_without_reaching_the_guest(self):
        self.tool('brew', 'raise RuntimeError("core targets must not invoke brew")')
        self.tool('cargo', """import json,os,sys
from pathlib import Path
with Path(os.environ['ENTRYPOINT_LOG']).open('a') as log:
 log.write(json.dumps({'args':sys.argv[1:],'flags':os.getenv('CARGO_ENCODED_RUSTFLAGS')})+'\\n')
""")
        self.env['HOST_RUSTFLAGS'] = '-Lnative=/installed library'+'\x1f'+'-Clink-arg=-rpath'
        result = subprocess.run(['make', '--no-print-directory', 'cargo-host', 'cargo-guest'],
            cwd=ROOT, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        host, guest = map(json.loads, self.log.read_text().splitlines())
        self.assertEqual(host['flags'], self.env['HOST_RUSTFLAGS'])
        self.assertIsNone(guest['flags'])

    def test_plain_acceptance_uses_the_existing_script_without_homebrew(self):
        self.tool('brew', 'raise RuntimeError("must not invoke brew")')
        self.tool('bash', '''import json,os,sys
from pathlib import Path
Path(os.environ['ENTRYPOINT_LOG']).write_text(json.dumps(sys.argv[1:]))
''')
        result = self.run_make('test-acceptance', ACCEPTANCE_GATES='m2-smoke-lifetimes')
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        self.assertEqual(json.loads(self.log.read_text()),
                         ['scripts/accept-native-build.sh', 'm2-smoke-lifetimes'])
