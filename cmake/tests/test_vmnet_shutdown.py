"""Exercise the production shutdown boundary with controlled vmnet results."""
import json
from pathlib import Path
import resource
import signal
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(sys.platform == 'darwin', 'requires Apple SDK and Blocks runtime')
class NativeShutdown(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        directory = Path(cls.directory.name)
        fixture = directory / 'shutdown.c'
        # Include the actual implementation so its private handle layout stays
        # authoritative. Only the two framework calls are replaced.
        fixture.write_text(f'#include {json.dumps(str(ROOT / "crates/clankerd-network/src/vmnet.c"))}\n' + r'''
static int mode;
vmnet_return_t vmnet_interface_set_event_callback(interface_ref interface, interface_event_t events,
                                                dispatch_queue_t queue, vmnet_interface_event_callback_t callback) {
  return VMNET_SUCCESS;
}
vmnet_return_t vmnet_stop_interface(interface_ref interface, dispatch_queue_t queue,
                                  void (^completion)(vmnet_return_t)) {
  if (mode == 1) return VMNET_FAILURE;
  completion(mode == 2 ? VMNET_FAILURE : VMNET_SUCCESS);
  return VMNET_SUCCESS;
}
int main(int argc, char **argv) {
  mode = atoi(argv[1]);
  clankerd_interface *handle = calloc(1, sizeof(*handle));
  handle->interface = (interface_ref)1;
  handle->queue = dispatch_queue_create("example.test.shutdown", NULL);
  clankerd_vmnet_stop(handle);
  return 0;
}
''')
        cls.executable = directory / 'shutdown'
        subprocess.run(['xcrun', 'clang', '-fblocks', '-mmacosx-version-min=15.0',
                        str(fixture), '-framework', 'vmnet', '-o', str(cls.executable)],
                       check=True, capture_output=True, text=True)

    def test_shutdown_returns_only_after_success(self):
        for mode in [0, 1, 2]:
            with self.subTest(mode=mode):
                result = subprocess.run([str(self.executable), str(mode)],
                                        capture_output=True, text=True, timeout=5,
                                        preexec_fn=lambda: resource.setrlimit(resource.RLIMIT_CORE, (0, 0)))
                if mode == 0:
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    self.assertEqual(result.returncode, -signal.SIGABRT, result.stderr)
                    self.assertIn('vmnet_stop_interface', result.stderr)
