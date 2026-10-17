"""Exercise bundle publication using real processes and disposable fake tools."""
import concurrent.futures
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("rust_build", Path(__file__).parents[1] / "rust-build.py")
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="clankerd build ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.output = self.root / "build/rust"
        tools = self.root / "tools"
        tools.mkdir()
        script = f'''#!{sys.executable}
import json, os
from pathlib import Path
import sys
name = Path(sys.argv[0]).name
if name == 'cargo':
    directory = Path(os.environ['FIXTURE_ROOT']) / 'cargo output'
    directory.mkdir(exist_ok=True)
    for binary in ['vmctl', 'clankerd-vmspawn', 'clankerd-guestd']:
        path = directory / binary
        path.write_text('compiled')
        print(json.dumps({{'reason': 'compiler-artifact', 'target': {{'name': binary}}, 'executable': str(path)}}))
elif name == 'rcodesign':
    if os.environ.get('FAIL_SIGN'):
        sys.exit(1)
    path = Path(sys.argv[-1])
    path.write_text(path.read_text() + ':signed')
'''
        for tool in ["cargo", "rcodesign", "zig", "tar", "patch", "make", "cc"]:
            path = tools / tool
            path.write_text(script)
            path.chmod(0o755)
        scripts = self.root / "scripts"
        scripts.mkdir()
        dependency = f'''#!{sys.executable}
import os, sys
from pathlib import Path
if os.environ.get('FAIL_DEPENDENCY'):
    sys.exit(1)
for name in ['mke2fs', 'e2fsck', 'resize2fs', 'debugfs', 'dhcpcd', 'dhcpcd.LICENSE']:
    (Path(sys.argv[1]) / name).write_text('dependency')
'''
        for name in ["build-e2fsprogs.sh", "build-dhcpcd.sh"]:
            path = scripts / name
            path.write_text(dependency)
            path.chmod(0o755)
        self.addCleanup(patch.stopall)
        patch.object(BUILD, "ROOT", self.root).start()
        patch.object(BUILD.platform, "system", return_value="Linux").start()
        patch.object(BUILD.platform, "machine", return_value="aarch64").start()
        patch.dict(os.environ, {"PATH": str(tools), "FIXTURE_ROOT": str(self.root), "BUILD_JOBS": "2"}).start()

    def assert_complete(self):
        self.assertTrue(self.output.is_symlink())
        for binary in ["vmctl", "clankerd-vmspawn"]:
            self.assertEqual((self.output / "darwin-arm64" / binary).read_text(), "compiled:signed")
        self.assertTrue((self.output / "linux-arm64/dhcpcd.LICENSE").is_file())

    def test_failed_signing_preserves_last_complete_bundle(self):
        BUILD.build(self.output)
        previous = self.output.resolve()
        with patch.dict(os.environ, {"FAIL_SIGN": "1"}):
            with self.assertRaises(Exception):
                BUILD.build(self.output)
        self.assertEqual(self.output.resolve(), previous)
        self.assert_complete()
        self.assertEqual(len(list(previous.parent.glob("bundle-*"))), 1)

    def test_failed_dependency_does_not_publish(self):
        with patch.dict(os.environ, {"FAIL_DEPENDENCY": "1"}):
            with self.assertRaises(Exception):
                BUILD.build(self.output)
        self.assertFalse(self.output.exists())

    def test_legacy_directory_is_retained(self):
        self.output.mkdir(parents=True)
        (self.output / "darwin-arm64").mkdir()
        (self.output / "darwin-arm64/old").write_text("previous")
        BUILD.build(self.output)
        self.assert_complete()
        legacy = list((self.output.parent / ".rust-generations").glob("legacy-*"))
        self.assertEqual((legacy[0] / "darwin-arm64/old").read_text(), "previous")

    def test_unrelated_output_directory_is_untouched(self):
        self.output.mkdir(parents=True)
        (self.output / "unrelated").write_text("keep")
        with self.assertRaisesRegex(RuntimeError, "not a build bundle"):
            BUILD.build(self.output)
        self.assertEqual((self.output / "unrelated").read_text(), "keep")

    def test_concurrent_builds_publish_complete_generations(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            list(executor.map(lambda _: BUILD.build(self.output), range(2)))
        self.assert_complete()
        for bundle in (self.output.parent / ".rust-generations").glob("bundle-*"):
            self.assertTrue((bundle / "darwin-arm64/vmctl").read_text().endswith(":signed"))


if __name__ == "__main__":
    unittest.main()
