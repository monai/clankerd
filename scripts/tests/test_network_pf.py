"""Contracts at the controller's JSON/OS boundary. No host PF changes occur."""
import contextlib
import importlib.util
import io
import json
import multiprocessing
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('network_pf', Path(__file__).parents[1] / 'clankerd-network-pf.py')
pf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pf)
NETWORK_A, NETWORK_B = 'a' * 64, 'b' * 64


def request(member='a', network_id=NETWORK_A, subnet='192.168.120.0/24', pid=123):
    return dict(action='register', socket=f'/tmp/example-{member}.net', pid=pid,
                subnet=subnet, prefix=None, network_id=network_id)


class Host:
    """Stateful stand-in for process inventory, native bridges and owned PF state."""
    def __init__(self):
        self.processes = {}
        self.bridges = {'192.168.120.1': 'bridge100', '192.168.121.1': 'bridge101'}
        self.references = {'41'}  # Another application's enable reference.
        self.next_reference = 100
        self.policy = ''
        self.root_rules = 'anchor "com.apple/*" all'
        self.fail = None

    def add(self, row, uid=501, program='/opt/clankerd/libexec/vmnet-helper'):
        self.processes[row['pid']] = f'{uid} {program} --socket {row["socket"]}'

    def run(self, args, text=None):
        if args[0] == '/bin/ps':
            return self.processes.get(int(args[-1]), '')
        if args[0] == '/sbin/route':
            return '    interface: ' + self.bridges.get(args[-1], 'en0') + '\n'
        if args[0] == '/sbin/ifconfig':
            return '\n'.join(f'    inet {ip} netmask 0xffffff00' for ip, bridge in self.bridges.items() if bridge == args[-1])
        if args == ['/sbin/pfctl', '-sr']:
            return self.root_rules
        if args == ['/sbin/pfctl', '-E']:
            token = str(self.next_reference)
            self.next_reference += 1
            self.references.add(token)
            return 'Token : ' + token
        if args[:2] == ['/sbin/pfctl', '-X']:
            if self.fail == 'release':
                self.fail = None
                raise RuntimeError('simulated reference release failure')
            self.references.remove(args[2])
            return ''
        if args[0] == '/sbin/pfctl' and '-f' in args:
            if '-n' in args:
                if self.fail == 'syntax':
                    self.fail = None
                    raise RuntimeError('simulated syntax rejection')
                return ''
            if self.fail == 'load':
                self.fail = None
                raise RuntimeError('simulated policy load failure')
            if args[args.index('-a') + 1] != 'com.apple/clankerd-networks':
                raise AssertionError('attempted to change an unrelated anchor')
            self.policy = text
            return ''
        raise AssertionError('unexpected OS operation: ' + repr(args))

    def boundaries(self):
        marks = dict(re.findall(r'anchor "mark-[^"]+" in on (bridge\d+) all tag "([^"]+)"', self.policy))
        outgoing = re.findall(r'block drop out quick on (bridge\d+) all tagged "([^"]+)"', self.policy)
        return {(source, target) for source, tag in marks.items() for target, origin in outgoing if origin == tag}


class Controller:
    def __init__(self, directory, host):
        self.directory, self.host = Path(directory), host

    def execute(self, row, uid=501):
        with patch.object(pf, 'STATE', self.directory), patch.object(pf.sys, 'platform', 'darwin'), \
             patch.object(pf.os, 'geteuid', return_value=0), patch.dict(os.environ, SUDO_UID=str(uid)), \
             patch.object(pf, 'command', side_effect=self.host.run), \
             patch.object(pf.sys, 'stdin', io.StringIO(json.dumps(row))), \
             contextlib.redirect_stdout(io.StringIO()):
            pf.main()

    def state(self):
        path = self.directory / 'state.json'
        return json.loads(path.read_text()) if path.exists() else {'records': {}, 'token': None}


def register_worker(directory, rows, index):
    host = Host()
    for row in rows:
        host.add(row)
    Controller(directory, host).execute(rows[index])


class CommandContract(unittest.TestCase):
    def test_returns_output_and_reports_unsuccessful_commands(self):
        self.assertEqual(pf.command([sys.executable, '-c', 'import sys;print("foo");print("bar",file=sys.stderr)']), 'foo\nbar\n')
        with self.assertRaisesRegex(RuntimeError, 'example failure'):
            pf.command([sys.executable, '-c', 'import sys;sys.stderr.write("example failure");sys.exit(1)'])


class HelperIdentityContract(unittest.TestCase):
    def test_only_the_callers_helper_at_the_exact_socket_is_live(self):
        record = dict(pid=123, uid=501, socket='/tmp/example-a.net')
        cases = [
            ('501 /opt/clankerd/libexec/vmnet-helper --socket /tmp/example-a.net', True),
            ('501 /opt/vmnet-helper/bin/vmnet-helper --verbose --socket /tmp/example-a.net --other foo', True),
            ('502 /opt/clankerd/libexec/vmnet-helper --socket /tmp/example-a.net', False),
            ('501 /opt/clankerd/libexec/not-vmnet-helper --socket /tmp/example-a.net', False),
            ('501 /opt/clankerd/libexec/vmnet-helper --socket /tmp/example-a.net-extra', False),
            ('501 /opt/clankerd/libexec/vmnet-helper --other /tmp/example-a.net', False),
            ('', False), ('foo', False), ('foo /opt/vmnet-helper --socket /tmp/example-a.net', False),
        ]
        for output, expected in cases:
            with self.subTest(output=output), patch.object(pf, 'command', return_value=output):
                self.assertEqual(pf.alive(record), expected)
        with patch.object(pf, 'command', side_effect=RuntimeError('no such process')):
            self.assertFalse(pf.alive(record))


class RegistryContract(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='clankerd-pf-contract-')
        self.addCleanup(self.temporary.cleanup)
        self.host = Host()
        self.controller = Controller(self.temporary.name, self.host)

    def register(self, row):
        self.host.add(row)
        self.controller.execute(row)

    def remove(self, row, uid=501):
        self.controller.execute(dict(action='remove', socket=row['socket']), uid)

    def test_membership_preserves_both_network_boundaries_until_last_member_stops(self):
        a, b = request(), request('b', pid=124)
        c = request('c', NETWORK_B, '192.168.121.0/24', 125)
        self.register(a)
        self.register(b)
        self.register(c)
        self.assertEqual(self.host.boundaries(), {('bridge100', 'bridge101'), ('bridge101', 'bridge100')})
        self.remove(a)
        self.assertEqual({r['socket'] for r in self.controller.state()['records'].values()}, {b['socket'], c['socket']})
        self.assertEqual(self.host.boundaries(), {('bridge100', 'bridge101'), ('bridge101', 'bridge100')})
        self.remove(b)
        self.assertEqual(self.host.boundaries(), set())
        self.remove(c)
        self.assertEqual(self.controller.state(), {'records': {}, 'token': None})
        self.assertEqual(self.host.references, {'41'})

    def test_distinct_resources_cannot_join_one_bridge_even_with_identical_addresses(self):
        self.register(request())
        before = self.controller.state()
        for changes in ({'network_id': NETWORK_B}, {'network_id': NETWORK_A, 'prefix': 'fd42:1::'}):
            row = dict(request('b', pid=124), **changes)
            self.host.add(row)
            with self.subTest(changes=changes), self.assertRaisesRegex(ValueError, 'conflict'):
                self.controller.execute(row)
            self.assertEqual(self.controller.state(), before)
            self.assertEqual(self.host.boundaries(), set())

    def test_one_resource_cannot_span_distinct_native_bridges(self):
        self.register(request())
        row = request('b', NETWORK_A, '192.168.121.0/24', 124)
        self.host.add(row)
        with self.assertRaisesRegex(ValueError, 'conflict'):
            self.controller.execute(row)
        self.assertEqual(len(self.controller.state()['records']), 1)

    def test_failed_registration_preserves_existing_policy_registry_and_references(self):
        for existing in (False, True):
            for failure in ('syntax', 'load', 'write', 'rename'):
                with self.subTest(existing=existing, failure=failure), tempfile.TemporaryDirectory() as directory:
                    host = Host()
                    controller = Controller(directory, host)
                    if existing:
                        a = request()
                        host.add(a)
                        controller.execute(a)
                    state, policy, references = controller.state(), host.policy, set(host.references)
                    b = request('b', NETWORK_B, '192.168.121.0/24', 124)
                    host.add(b)
                    host.fail = failure
                    method = 'write_text' if failure == 'write' else 'replace'
                    original = getattr(Path, method)
                    def fail_file(path, *args, **kwargs):
                        if path.name == 'state.tmp':
                            raise OSError('simulated registry ' + failure + ' failure')
                        return original(path, *args, **kwargs)
                    file_patch = patch.object(Path, method, fail_file) if failure in ('write', 'rename') else contextlib.nullcontext()
                    with file_patch, self.assertRaises((RuntimeError, OSError)):
                        controller.execute(b)
                    self.assertEqual(controller.state(), state)
                    self.assertEqual(host.policy.strip(), policy.strip())
                    self.assertEqual(host.references, references)

    def test_stale_helpers_are_pruned_and_replacement_retains_resource_identity(self):
        row = request()
        self.register(row)
        self.host.processes.clear()
        replacement = dict(row, pid=124)
        self.register(replacement)
        records = list(self.controller.state()['records'].values())
        self.assertEqual(len(records), 1)
        self.assertEqual((records[0]['pid'], records[0]['network_id']), (124, NETWORK_A))
        self.host.processes.clear()
        self.remove(row)
        self.assertEqual(self.controller.state(), {'records': {}, 'token': None})
        self.assertEqual(self.host.references, {'41'})

    def test_remove_is_idempotent_and_cannot_remove_another_callers_attachment(self):
        row = request()
        self.register(row)
        before = self.controller.state()
        self.remove(row, uid=502)
        self.assertEqual(self.controller.state(), before)
        self.remove(row)
        self.remove(row)
        self.assertEqual(self.controller.state(), {'records': {}, 'token': None})
        self.assertEqual(self.host.references, {'41'})

    def test_repeated_registration_has_one_member_and_one_owned_reference(self):
        row = request()
        self.register(row)
        self.register(row)
        self.assertEqual(len(self.controller.state()['records']), 1)
        self.assertEqual(len(self.host.references), 2)
        self.remove(row)
        self.assertEqual(self.host.references, {'41'})

    def test_reference_release_failure_keeps_a_retryable_record(self):
        row = request()
        self.register(row)
        self.host.fail = 'release'
        with self.assertRaisesRegex(RuntimeError, 'release'):
            self.remove(row)
        self.assertEqual(self.controller.state()['records'], {})
        self.assertEqual(len(self.host.references), 2)
        self.remove(row)
        self.assertEqual(self.controller.state(), {'records': {}, 'token': None})
        self.assertEqual(self.host.references, {'41'})

    def test_concurrent_members_are_not_lost(self):
        rows = [request(str(index), pid=123 + index) for index in range(4)]
        context = multiprocessing.get_context('fork')
        processes = [context.Process(target=register_worker, args=(self.temporary.name, rows, index)) for index in range(4)]
        try:
            for process in processes:
                process.start()
            for process in processes:
                process.join(15)
                self.assertEqual(process.exitcode, 0)
        finally:
            for process in processes:
                if process.is_alive():
                    process.terminate()
                process.join()
        self.assertEqual({r['socket'] for r in self.controller.state()['records'].values()}, {r['socket'] for r in rows})

    def test_host_policy_without_the_required_anchor_fails_before_registration(self):
        self.host.root_rules = ''
        row = request()
        self.host.add(row)
        with self.assertRaisesRegex(RuntimeError, 'anchor'):
            self.controller.execute(row)
        self.assertEqual(self.controller.state(), {'records': {}, 'token': None})
        self.assertEqual(self.host.policy, '')
        self.assertEqual(self.host.references, {'41'})


class ValidationContract(unittest.TestCase):
    def test_execution_requires_root_on_macos(self):
        for platform, uid in (('linux', 0), ('darwin', 501)):
            with self.subTest(platform=platform, uid=uid), \
                 patch.object(pf.sys, 'platform', platform), \
                 patch.object(pf.os, 'geteuid', return_value=uid), \
                 self.assertRaisesRegex(RuntimeError, 'root on macOS'):
                pf.main()

    def test_registration_requires_an_owned_helper_and_stable_resource_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            host = Host()
            controller = Controller(directory, host)
            row = request()
            for changes in ({'pid': 0}, {'network_id': None}, {'network_id': 'foo'}, {'socket': 'relative'}, {'action': 'foo'}):
                with self.subTest(changes=changes), self.assertRaises(ValueError):
                    controller.execute(dict(row, **changes))
            for uid, program in ((502, '/opt/clankerd/libexec/vmnet-helper'), (501, '/bin/foo')):
                host.add(row, uid, program)
                with self.subTest(uid=uid, program=program), self.assertRaisesRegex(ValueError, 'helper'):
                    controller.execute(row)
            self.assertEqual(host.references, {'41'})

    def test_addressing_accepts_only_aligned_private_subnets_and_ula_prefixes(self):
        host = Host()
        row = request()
        host.add(row)
        valid = [('10.0.0.0/8', None), ('172.16.0.0/12', 'fd42::'), ('192.168.120.0/30', 'fd42:1::')]
        with patch.object(pf, 'command', side_effect=host.run):
            for subnet, prefix in valid:
                gateway = str(pf.ipaddress.ip_network(subnet).network_address + 1)
                host.bridges[gateway] = 'bridge100'
                _, record = pf.validate(dict(row, subnet=subnet, prefix=prefix), 501)
                self.assertEqual(record['subnet'], subnet)
            for subnet in ('1.1.1.0/24', '192.168.120.1/24', '192.168.120.0/31'):
                with self.subTest(subnet=subnet), self.assertRaises(ValueError):
                    pf.validate(dict(row, subnet=subnet), 501)
            for prefix in ('fd42::1', '2001:db8::'):
                with self.subTest(prefix=prefix), self.assertRaises(ValueError):
                    pf.validate(dict(row, prefix=prefix), 501)

    def test_bridge_discovery_requires_a_native_bridge_owning_the_gateway(self):
        host = Host()
        subnet = pf.ipaddress.ip_network('192.168.120.0/24')
        with patch.object(pf, 'command', side_effect=host.run), patch.object(pf.time, 'sleep'):
            self.assertEqual(pf.bridge_for(subnet), 'bridge100')
            def wrong_owner(args, text=None):
                if args[0] == '/sbin/ifconfig':
                    return '    inet 192.168.121.1 netmask 0xffffff00'
                return host.run(args, text)
            with patch.object(pf, 'command', side_effect=wrong_owner), self.assertRaisesRegex(RuntimeError, 'gateway'):
                pf.bridge_for(subnet)
            host.bridges.clear()
            with self.assertRaisesRegex(RuntimeError, 'gateway'):
                pf.bridge_for(subnet)


if __name__ == '__main__':
    unittest.main()
