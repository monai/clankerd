#!/usr/bin/env python3
"""Observe the existing build-test fixture; retain Make's full decision trace."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
os.chdir(root)
(root / 'build').mkdir(exist_ok=True)
evidence = Path(tempfile.mkdtemp(prefix='make-diagnostic-', dir=root / 'build'))
spec = importlib.util.spec_from_file_location('build_graph', root / 'scripts/tests/test_build_graph.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
case = module.ArtifactGraph()


def observe(path):
    if not path.exists():
        return None
    return {'mtime_ns': path.stat().st_mtime_ns,
            'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
            'size': path.stat().st_size}


try:
    case.setUp()
    environment = dict(case.env, MAKEFLAGS='--debug=v')
    source = case.root / 'cargo/aarch64-apple-darwin/release/vmctl'
    installed = case.root / 'build/artifact/darwin-arm64/vmctl'
    version = subprocess.run(['make', '--version'], env=environment,
                             text=True, capture_output=True, check=True).stdout
    print(f'[1/4] {platform.system()} {platform.machine()}; {version.splitlines()[0]}')
    print(f'Make: {shutil.which("make", path=environment["PATH"])}')
    print(f'Evidence: {evidence}')
    with (evidence / 'make.log').open('w') as log:
        log.write(version)
        for step, name, overrides in [
            (2, 'initial build', {}),
            (3, 'changed host, signing forced to fail',
             {'HOST_PAYLOAD': 'changed host', 'FAIL_SIGN': '1'}),
            (4, 'changed host, signing allowed', {'HOST_PAYLOAD': 'completed host'}),
        ]:
            before_source, before_installed = observe(source), observe(installed)
            result = subprocess.run(case.args, cwd=case.root,
                                    env=dict(environment, **overrides),
                                    text=True, capture_output=True)
            after_source, after_installed = observe(source), observe(installed)
            trace = result.stdout + result.stderr
            log.write(f'\n--- {name}\n{trace}\n')
            log.write(json.dumps({'exit': result.returncode,
                'before_source': before_source, 'after_source': after_source,
                'before_installed': before_installed, 'after_installed': after_installed}) + '\n')
            signing = any('rcodesign sign' in line and 'io.clankerd.vmctl' in line
                          for line in trace.splitlines())
            print(f'[{step}/4] {name}: exit={result.returncode}; signing command={signing}')
            print(f'  Cargo output changed={before_source != after_source}; '
                  f'installed changed={before_installed != after_installed}')
            if after_source and after_installed:
                print(f'  mtime_ns: source={after_source["mtime_ns"]} '
                      f'installed={after_installed["mtime_ns"]}')
                print(f'  sha256: source={after_source["sha256"][:12]} '
                      f'installed={after_installed["sha256"][:12]}')
            decisions = [line.strip().replace(str(case.root), '<fixture>')
                         for line in trace.splitlines()
                         if 'build/artifact/darwin-arm64/vmctl' in line and (
                             ('Prerequisite' in line and '/cargo/' in line)
                             or 'No need' in line or 'Must remake' in line)]
            for line in decisions[-3:]:
                print('  ' + line[:180])
            if step == 2 and result.returncode:
                print('Initial build failed; remaining observations cannot be collected. See make.log.')
                break
finally:
    case.doCleanups()
