#!/usr/bin/env python3
"""Native, idle-host probe. No product changes or fault-injection hooks required."""
import argparse
import hashlib
import json
import pathlib
import platform
import subprocess
import tempfile
import time
import uuid

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--client', required=True, type=pathlib.Path)
p.add_argument('--inject-cleanup-failure', action='store_true',
               help='requires idle development host; restarts helper to recover probe PF state')
a = p.parse_args()
if platform.system() != 'Darwin':
    p.error('requires macOS')
client = a.client.resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='clankerd-network-probe-'))
print(f'Evidence: {root}', flush=True)
seq = 0

def run(argv, check=True):
    global seq
    seq += 1
    started = time.monotonic()
    try:
        r = subprocess.run([str(x) for x in argv], capture_output=True, text=True, timeout=45)
        result = dict(argv=[str(x) for x in argv], code=r.returncode,
                      stdout=r.stdout, stderr=r.stderr, seconds=time.monotonic()-started)
    except subprocess.TimeoutExpired as e:
        result = dict(argv=[str(x) for x in argv], timeout=True, seconds=time.monotonic()-started)
        (root / f'{seq:03}.json').write_text(json.dumps(result, indent=2))
        raise RuntimeError(f'command timed out: {argv}') from e
    (root / f'{seq:03}.json').write_text(json.dumps(result, indent=2))
    if check and r.returncode:
        raise RuntimeError(f'command failed; see {seq:03}.json: {r.stderr}')
    return r

state = '/var/run/clankerd-network-pf/state.json'
blocker = '/var/run/clankerd-network-pf/state.tmp'

def snapshot(label):
    r = run(['sudo', '-n', 'cat', state], check=False)
    (root / f'{label}-state.json').write_text(r.stdout)
    run(['sudo', '-n', '/sbin/pfctl', '-a', 'com.apple/clankerd-networks', '-sr'], check=False)
    return json.loads(r.stdout) if r.returncode == 0 else None

run(['sudo', '-v'])
run(['git', 'rev-parse', 'HEAD'])
patch = run(['git', 'diff', 'HEAD']).stdout
(root / 'source.patch').write_text(patch)
(root / 'manifest.json').write_text(json.dumps(dict(
    client=str(client), client_sha256=hashlib.sha256(client.read_bytes()).hexdigest(),
    platform=platform.platform(), injection=a.inject_cleanup_failure), indent=2))
run(['shasum', '-a', '256', '/Library/PrivilegedHelperTools/clankerd-network'])
run(['sw_vers'])
before = snapshot('before')
if before and (before.get('records') or before.get('token')):
    raise SystemExit('Requires empty PF records and no token. Stop VMs and resolve existing cleanup first.')
# Never overwrite an existing temporary file or directory.
if run(['sudo', '-n', 'test', '-e', blocker], check=False).returncode == 0:
    raise SystemExit(f'Refusing to touch existing {blocker}')
key = str(root / 'attachment')
netid = uuid.uuid4().hex + uuid.uuid4().hex
record_key = f'{__import__("os").getuid()}:{key}'
generation = None
blocked = False
summary = {}
try:
    generation = run([client, 'create', key, str(uuid.uuid4()), netid]).stdout.strip()
    # The create subprocess has exited; no Attach request was sent.
    observations = []
    for delay in (0, 6, 10):
        time.sleep(delay)
        observations.append(run([client, 'status', key, generation]).stdout.strip())
    summary['unattached_status_at_0_6_16_seconds'] = observations
    summary['pending_attachment_retained_for_observation_window'] = all(
        x == 'connected=true' for x in observations)
    created = snapshot('created')
    if not created or record_key not in created.get('records', {}):
        raise RuntimeError('No probe PF record: setup did not establish the intended condition')
    if a.inject_cleanup_failure:
        # rename(state.tmp, state.json) cannot succeed when state.tmp is a directory.
        run(['sudo', '-n', 'mkdir', blocker])
        blocked = True
        failed = run([client, 'stop', key, generation], check=False)
        summary['injected_stop_exit'] = failed.returncode
        snapshot('failed-stop')
        run(['sudo', '-n', 'rmdir', blocker])
        blocked = False
        retry = run([client, 'stop', key, generation], check=False)
        after = snapshot('retry-stop')
        summary['retry_stop_exit'] = retry.returncode
        summary['retry_left_pf_record'] = bool(after and record_key in after.get('records', {}))
        summary['cleanup_retry_gap_observed'] = (
            failed.returncode != 0 and retry.returncode == 0 and summary['retry_left_pf_record'])
    else:
        run([client, 'stop', key, generation])
finally:
    if blocked:
        run(['sudo', '-n', 'rmdir', blocker])
    if generation:
        run([client, 'stop', key, generation], check=False)
    remaining = snapshot('before-recovery')
    if remaining and record_key in remaining.get('records', {}):
        if a.inject_cleanup_failure and set(remaining['records']) == {record_key}:
            run(['sudo', '-n', 'launchctl', 'kickstart', '-k', 'system/io.clankerd.network'])
            time.sleep(2)
        else:
            summary['cleanup_requires_operator'] = True
    final = snapshot('final')
    summary['final_probe_record_absent'] = bool(final is not None and record_key not in final.get('records', {}))
    summary['final_token'] = final.get('token') if final is not None else 'unknown'
    (root / 'summary.json').write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary, indent=2))
    print(f'Return evidence directory: {root}')
