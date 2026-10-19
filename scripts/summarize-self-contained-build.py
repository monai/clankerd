#!/usr/bin/env python3
"""Print a bounded pasteable report from retained acceptance evidence; run no VMs."""
import json
from pathlib import Path
import re
import sys


def failure_excerpt(text):
    test_failures = re.findall(
        r'(?ms)^((?:FAIL|ERROR): test_[^\n]+\n.*?)(?=^={10,}$|\Z)', text)
    if test_failures:
        report = []
        for failure in test_failures:
            lines = failure.splitlines()
            report.append(lines[0])
            for line in lines[1:]:
                if (line.lstrip().startswith(('File ', 'self.run_make(', 'AssertionError:',
                                              'RuntimeError:', 'OSError:'))):
                    report.append(line.strip())
        return report
    # Cleanup dumps many machines' console logs after the actual failing assertion.
    # A plain tail hides that assertion beneath unrelated vsock/DHCP messages.
    body = re.split(r'(?m)^== /(?:private/)?tmp/', text, maxsplit=1)[0]
    lines = body.splitlines()
    failures = [i for i, line in enumerate(lines)
                if line.startswith(('FAIL:', 'Error:', 'error:', 'AssertionError:', 'TIMEOUT'))]
    end = failures[-1] + 1 if failures else len(lines)
    return lines[max(0, end - 20):end]


def summarize(evidence):
    result = json.loads((evidence / 'result.json').read_text())
    lines = [f'[1/3] Evidence: {evidence}',
             f'Overall: {result["status"]} (exit {result["exit_code"]})',
             '[2/3] Results']
    management = result.get('management')
    if management:
        lines.append('Management: ' + management['status'])
        if management.get('error'):
            lines.append('Management error: ' + management['error'])
    gates = result.get('gates', [])
    lines.extend(gates)
    lines.append('[3/3] Failure context (verbose diagnostics remain in the evidence directory)')
    failed = [gate.split()[-1] for gate in gates if gate.startswith('FAIL')]
    for gate in failed:
        # Gate names come from the fixed native acceptance list, never a path.
        if not re.fullmatch(r'm2-[a-z-]+', gate):
            continue
        log = evidence / (gate + '.log')
        lines.append('--- ' + gate)
        if log.is_file():
            lines.extend(failure_excerpt(log.read_text(errors='replace')))
        else:
            lines.append('Missing gate log: ' + str(log))
    if result['status'] != 'PASS' and not failed:
        log = evidence / 'acceptance.log'
        if log.is_file():
            lines.extend(failure_excerpt(log.read_text(errors='replace')))
    # Bound giant command lines too, keeping the whole original in its log.
    lines = [line[:500] + (' [truncated; see log]' if len(line) > 500 else '') for line in lines]
    return '\n'.join(lines) + '\n'


def main():
    if sys.argv[1:] == ['--progress']:
        for line in sys.stdin:
            if re.match(r'^(\[\d+/\d+\]|STILL RUNNING|PASS\t|FAIL\t|TIMEOUT|Evidence retained:)', line):
                print(line, end='', flush=True)
        return
    if len(sys.argv) > 1:
        evidence = Path(sys.argv[1]).resolve()
    else:
        root = Path(__file__).resolve().parents[1]
        results = list((root / 'build').glob('acceptance-*/result.json'))
        if not results:
            sys.exit('No completed acceptance result found in build/acceptance-*/result.json')
        evidence = max(results, key=lambda path: path.stat().st_mtime_ns).parent
    report = summarize(evidence)
    (evidence / 'paste-summary.txt').write_text(report)
    print(report, end='')


if __name__ == '__main__':
    main()
