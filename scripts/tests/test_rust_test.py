import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("rust_test", Path(__file__).parents[1] / "rust-test.py")
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.log = self.root / "calls.jsonl"
        self.fake = self.root / "cargo"
        self.fake.write_text('''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["RECORD"], "a") as log:
    log.write(json.dumps({"args": args, "env": dict(os.environ)}) + "\\n")
if "metadata" in args:
    print(json.dumps({"target_directory": os.environ["META_TARGET"]}))
elif "show-env" in args:
    print("export RUSTFLAGS='-Cinstrument-coverage -Cllvm-args=-runtime-counter-relocation'")
    print("export LLVM_PROFILE_FILE='/profile-%p.profraw'")
elif "build" in args:
    for name in ["clankerd-guestd", "clankerd-vmspawn"]:
        print(json.dumps({"reason": "compiler-artifact", "target": {"name": name}, "executable": "/resolved/" + name}))
''')
        self.fake.chmod(0o755)
        tools = self.root / "tools"
        tools.mkdir()
        for name in ("mke2fs", "e2fsck", "resize2fs", "debugfs"):
            tool = tools / name
            tool.write_text("#!/bin/sh\nexit 0\n")
            tool.chmod(0o755)
        env = {"CLANKERD_TEST_BOOT_DIR": str(tools), "CARGO": str(self.fake), "RECORD": str(self.log),
               "META_TARGET": str(self.root / "configured-target"),
               "COVERAGE_OUT": str(self.root / "reports")}
        self.environment = patch.dict(os.environ, env, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_native_vm_checks_build_the_selected_bundle_first(self):
        with patch.dict(os.environ, {"OUT": "custom-bundle"}), \
             patch.object(runner.subprocess, "run") as build, \
             patch.object(runner, "cargo") as cargo:
            runner.main("test-native", "Darwin")
            expected = str(runner.ROOT / "custom-bundle")
            self.assertEqual(build.call_args.kwargs["env"]["RUST_OUT"], expected)
            self.assertEqual(cargo.call_args.kwargs["env"]["OUT"], expected)
            self.assertIn("--ignored", cargo.call_args.args)
        with patch.object(runner.subprocess, "run") as build:
            with self.assertRaisesRegex(RuntimeError, "Native VM tests require"):
                runner.main("test-native", "Linux")
            build.assert_not_called()

    def test_linux_prebuild_paths_reach_suite(self):
        runner.main("test", "Linux")
        build, test = self.calls()
        self.assertEqual(build["args"][0], "build")
        self.assertIn("--locked", build["args"])
        self.assertEqual(test["args"], ["test", "--locked", "--workspace"])
        self.assertEqual(json.loads(test["env"]["CLANKERD_TEST_BINARIES"]),
                         {name: "/resolved/" + name for name in ["clankerd-guestd", "clankerd-vmspawn"]})

    def test_path_tools_are_published_to_rust_harness(self):
        env = {"PATH": str(self.root / "tools")}
        with patch.object(runner, "SYSTEM_TOOL_DIRS", ()):
            with runner.linux_test_environment(env) as resolved:
                self.assertEqual(resolved["CLANKERD_TEST_BOOT_DIR"], str(self.root / "tools"))

    def test_split_tool_directories_get_temporary_links(self):
        second = self.root / "other-tools"
        second.mkdir()
        (self.root / "tools/debugfs").rename(second / "debugfs")
        env = {"PATH": os.pathsep.join([str(self.root / "tools"), str(second)])}
        with patch.object(runner, "SYSTEM_TOOL_DIRS", ()):
            with runner.linux_test_environment(env) as resolved:
                directory = Path(resolved["CLANKERD_TEST_BOOT_DIR"])
                self.assertTrue((directory / "debugfs").is_file())
                self.assertTrue((directory / "mke2fs").is_file())
            self.assertFalse(directory.exists())

    def test_missing_linux_tools_fail_before_cargo(self):
        with patch.dict(os.environ, {"CLANKERD_TEST_BOOT_DIR": str(self.root / "missing")}):
            for operation in ("test", "coverage"):
                with self.assertRaisesRegex(RuntimeError, "Install native e2fsprogs"):
                    runner.main(operation, "Linux")
        self.assertFalse(self.log.exists())

    def test_mac_never_builds_local_linux_guest(self):
        runner.main("test", "Darwin")
        calls = self.calls()
        self.assertIn("--exclude", calls[0]["args"])
        self.assertTrue(all(call["args"][0] == "test" for call in calls))
        self.assertFalse(any("CLANKERD_TEST_BINARIES" in call["env"] for call in calls))
        with self.assertRaisesRegex(RuntimeError, "Linux execution context"):
            runner.main("test-linux", "Darwin")

    def test_coverage_directory_and_instrumentation_survive_all_phases(self):
        custom = self.root / "custom coverage"
        with patch.dict(os.environ, {"CARGO_LLVM_COV_TARGET_DIR": str(custom)}):
            runner.main("coverage", "Linux")
        calls = self.calls()
        for call in calls[1:]:
            self.assertEqual(call["env"]["CARGO_LLVM_COV_TARGET_DIR"], str(custom))
            self.assertEqual(call["env"]["CARGO_TARGET_DIR"], str(custom))
        helper = next(call for call in calls if "build" in call["args"])
        collect = next(call for call in calls if "test" in call["args"])
        self.assertEqual(helper["env"]["RUSTFLAGS"], collect["env"]["RUSTFLAGS"])
        self.assertIn("instrument-coverage", helper["env"]["RUSTFLAGS"])
        self.assertEqual(helper["env"]["LLVM_PROFILE_FILE"], collect["env"]["LLVM_PROFILE_FILE"])
        self.assertTrue(all("+nightly-2026-10-08" == call["args"][0] for call in calls))


if __name__ == "__main__":
    unittest.main()
