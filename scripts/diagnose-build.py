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
import sys
import tempfile
import unittest

root = Path(__file__).resolve().parent.parent
os.chdir(root)
(root / 'build').mkdir(exist_ok=True)
evidence = Path(tempfile.mkdtemp(prefix='make-diagnostic-', dir=root / 'build'))
if sys.argv[1:] == ['--regressions']:
    print(f'[1/2] Script regressions; evidence: {evidence}', flush=True)
    suite = unittest.defaultTestLoader.discover(str(root / 'scripts/tests'))
    log_path = evidence / 'regressions.log'
    with log_path.open('w') as log:
        result = unittest.TextTestRunner(stream=log, verbosity=2).run(suite)
    print(f'[2/2] {"PASS" if result.wasSuccessful() else "FAIL"}: '
          f'{result.testsRun} tests; {len(result.failures)} failures; {len(result.errors)} errors')
    if not result.wasSuccessful():
        summary_spec = importlib.util.spec_from_file_location(
            'acceptance_summary', root / 'scripts/summarize-native-build.py')
        summary = importlib.util.module_from_spec(summary_spec)
        summary_spec.loader.exec_module(summary)
        for line in summary.failure_excerpt(log_path.read_text()):
            print(line[:500])
    sys.exit(0 if result.wasSuccessful() else 1)
if sys.argv[1:]:
    sys.exit('Usage: diagnose-build.py [--regressions]')
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
    signing_log = case.root / 'signing.log'
    environment = dict(case.env, MAKEFLAGS='--debug=v', GRAPH_SIGN_LOG=str(signing_log))
    source = case.root / 'cargo/aarch64-apple-darwin/release/vmctl'
    installed = case.root / 'build/artifact/darwin-arm64/vmctl'
    version = subprocess.run(['make', '--version'], env=environment,
                             text=True, capture_output=True, check=True).stdout
    runs = [
        ('initial build', {}),
        ('changed host, signing forced to fail',
         {'HOST_PAYLOAD': 'changed host', 'FAIL_SIGN': '1'}),
        ('changed host, signing allowed', {'HOST_PAYLOAD': 'completed host'}),
    ] + [(f'repeated update {attempt}', {'HOST_PAYLOAD': f'host update {attempt}'})
         for attempt in range(1, 13)]
    total = len(runs) + 1
    print(f'[1/{total}] {platform.system()} {platform.machine()}; {version.splitlines()[0]}')
    print(f'Make: {shutil.which("make", path=environment["PATH"])}')
    print(f'Evidence: {evidence}')
    with (evidence / 'make.log').open('w') as log:
        log.write(version)
        for step, (name, overrides) in enumerate(runs, 2):
            before_source, before_installed = observe(source), observe(installed)
            signing_log.write_text('')
            result = subprocess.run(case.args, cwd=case.root,
                                    env=dict(environment, **overrides),
                                    text=True, capture_output=True)
            after_source, after_installed = observe(source), observe(installed)
            trace = result.stdout + result.stderr
            log.write(f'\n--- {name}\n{trace}\n')
            log.write(json.dumps({'exit': result.returncode,
                'before_source': before_source, 'after_source': after_source,
                'before_installed': before_installed, 'after_installed': after_installed}) + '\n')
            signing = 'io.clankerd.vmctl' in signing_log.read_text()
            if step >= 5 and result.returncode == 0 and after_source and after_installed:
                if after_source['sha256'] == after_installed['sha256']:
                    same_second = (after_source['mtime_ns'] // 1_000_000_000 ==
                                   before_installed['mtime_ns'] // 1_000_000_000)
                    print(f'[{step}/{total}] {name}: matching output; '
                          f'signing={signing}; same second={same_second}')
                    continue
            print(f'[{step}/{total}] {name}: exit={result.returncode}; signing command={signing}')
            print(f'  Cargo output changed={before_source != after_source}; '
                  f'installed changed={before_installed != after_installed}')
            if after_source and after_installed:
                print(f'  mtime_ns: source={after_source["mtime_ns"]} '
                      f'installed={after_installed["mtime_ns"]}')
                print(f'  sha256: source={after_source["sha256"][:12]} '
                      f'installed={after_installed["sha256"][:12]}')
                if before_installed:
                    same_second = (after_source['mtime_ns'] // 1_000_000_000 ==
                                   before_installed['mtime_ns'] // 1_000_000_000)
                    print(f'  Source and previous installation in same second={same_second}')
            decisions = [line.strip().replace(str(case.root), '<fixture>')
                         for line in trace.splitlines()
                         if '/artifact/darwin-arm64/vmctl' in line and (
                             ('Prerequisite' in line and '/cargo/' in line)
                             or 'No need' in line or 'Must remake' in line)]
            for line in decisions[-3:]:
                print('  ' + line[:180])
            if step == 2 and result.returncode:
                print('Initial build failed; remaining observations cannot be collected. See make.log.')
                break
            if step >= 4 and (result.returncode or not after_source or not after_installed
                              or after_source['sha256'] != after_installed['sha256']):
                print('Update did not install matching output; full trace retained in make.log.')
                break
finally:
    case.doCleanups()
