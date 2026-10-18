#!/usr/bin/env python3
"""Native Rust checks and Linux integration/coverage orchestration."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
COVERAGE_TOOLCHAIN = "nightly-2026-10-08"
SYSTEM_TOOL_DIRS = ("/usr/sbin", "/sbin", "/usr/bin")


def cargo(*args, env=None, capture=False, toolchain=None, check=True):
    command = [os.environ.get("CARGO", "cargo")]
    if toolchain:
        command.append("+" + toolchain)
    return subprocess.run(command + list(args), cwd=ROOT, env=env, check=check,
                          text=True, stdout=subprocess.PIPE if capture else None)


def prebuild(env, toolchain=None):
    process = cargo("build", "--locked", "--message-format=json", "-p", "clankerd-guestd",
                   "-p", "clankerd-vmspawn", "--bins", env=env, capture=True,
                   toolchain=toolchain, check=False)
    paths = {}
    for line in process.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            paths[message["target"]["name"]] = message["executable"]
        elif message.get("reason") == "compiler-message":
            print(message["message"].get("rendered", ""), file=sys.stderr, end="")
    process.check_returncode()
    for name in ("clankerd-guestd", "clankerd-vmspawn"):
        if name not in paths:
            raise RuntimeError(f"Cargo reported no executable for {name}")
    return dict(env, CLANKERD_TEST_BINARIES=json.dumps(paths))


def linux_required(system):
    if system != "Linux":
        raise RuntimeError("Linux guest integration tests require a Linux execution context; "
                           "run mise exec -- make test-linux on Linux. make test runs native Mac checks.")


@contextmanager
def linux_test_environment(env):
    names = ("mke2fs", "e2fsck", "resize2fs", "debugfs")
    explicit = env.get("CLANKERD_TEST_BOOT_DIR")
    paths = {}
    for name in names:
        if explicit:
            candidates = [Path(explicit) / name]
        else:
            candidates = [Path(directory) / name for directory in SYSTEM_TOOL_DIRS]
            found = shutil.which(name, path=env.get("PATH", os.defpath))
            if found:
                candidates.append(Path(found))
        path = next((path.absolute() for path in candidates
                     if path.is_file() and os.access(path, os.X_OK)), None)
        if path is None:
            location = f" in CLANKERD_TEST_BOOT_DIR={explicit}" if explicit else " on this host"
            raise RuntimeError(f"Linux tests require executable {name}{location}. "
                               "Install native e2fsprogs or set CLANKERD_TEST_BOOT_DIR "
                               "to a directory containing mke2fs, e2fsck, resize2fs and debugfs.")
        paths[name] = path
    directories = {path.parent for path in paths.values()}
    if len(directories) == 1:
        yield dict(env, CLANKERD_TEST_BOOT_DIR=str(directories.pop()))
    else:
        with tempfile.TemporaryDirectory(prefix="clankerd-test-tools-") as directory:
            for name, path in paths.items():
                (Path(directory) / name).symlink_to(path)
            yield dict(env, CLANKERD_TEST_BOOT_DIR=directory)


def coverage(env):
    toolchain = env.get("RUST_COVERAGE_TOOLCHAIN", COVERAGE_TOOLCHAIN)
    metadata = json.loads(cargo("metadata", "--format-version=1", "--no-deps", "--locked",
                                env=env, capture=True, toolchain=toolchain).stdout)
    target = Path(env.get("CARGO_LLVM_COV_TARGET_DIR",
                          str(Path(metadata["target_directory"]) / "llvm-cov-target")))
    if not target.is_absolute():
        target = ROOT / target
    env = dict(env, CARGO_TARGET_DIR=str(target), CARGO_LLVM_COV_TARGET_DIR=str(target),
               CARGO_BUILD_JOBS=env.get("COVERAGE_JOBS", "2"),
               LLVM_PROFILE_FILE_NAME="clankerd-%p-%m%c.profraw")
    env["RUSTFLAGS"] = env.get("RUSTFLAGS", "") + " -Cllvm-args=-runtime-counter-relocation -Zcoverage-options=branch"
    # Let llvm-cov supply the exact instrumentation environment used by helpers and tests.
    output = cargo("llvm-cov", "show-env", "--sh", env=env,
                   capture=True, toolchain=toolchain).stdout
    for line in output.splitlines():
        words = shlex.split(line)
        if len(words) == 2 and words[0] == "export" and "=" in words[1]:
            key, value = words[1].split("=", 1)
            env[key] = value
    cargo("llvm-cov", "clean", "--workspace", env=env, toolchain=toolchain)
    env = prebuild(env, toolchain)
    out = Path(env.get("COVERAGE_OUT", "build/coverage"))
    if not out.is_absolute():
        out = ROOT / out
    out.mkdir(parents=True, exist_ok=True)
    cargo("test", "--workspace", "--locked", "-j", env["CARGO_BUILD_JOBS"],
          env=env, toolchain=toolchain)
    cargo("llvm-cov", "report", "--json", "--output-path", str(out / "coverage.json"),
          env=env, toolchain=toolchain)
    cargo("llvm-cov", "report", env=env, toolchain=toolchain)
    cargo("llvm-cov", "report", "--html", "--output-dir", str(out), env=env, toolchain=toolchain)


def main(operation, system=None):
    system = system or platform.system()
    if system not in ("Linux", "Darwin"):
        raise RuntimeError(f"Unsupported test host: {system}")
    env = os.environ.copy()
    if env.get("BUILD_JOBS"):
        env["CARGO_BUILD_JOBS"] = env["BUILD_JOBS"]
    if operation == "coverage-setup":
        linux_required(system)
        subprocess.run(["rustup", "toolchain", "install", env.get("RUST_COVERAGE_TOOLCHAIN", COVERAGE_TOOLCHAIN),
                        "--profile", "minimal", "--component", "llvm-tools-preview"], check=True)
    elif operation == "coverage":
        linux_required(system)
        with linux_test_environment(env) as test_env:
            coverage(test_env)
    elif operation == "test-native":
        if system != "Darwin":
            raise RuntimeError("Native VM tests require Apple Silicon macOS and signed helpers from make rust")
        bundle = Path(env.get("OUT", env.get("RUST_OUT", str(ROOT / "build/rust"))))
        if not bundle.is_absolute():
            bundle = ROOT / bundle
        env["RUST_OUT"] = str(bundle)
        subprocess.run(["make", "build"],
                       cwd=ROOT, env=env, check=True)
        env["OUT"] = env["RUST_OUT"]
        env["DYLD_LIBRARY_PATH"] = str(bundle / "darwin-arm64/lib")
        cargo("test", "--locked", "-p", "vmctl", "--test", "native_bind_semantics",
              "--", "--ignored", "--test-threads=1", env=env)
    elif operation == "lint":
        cargo("fmt", "--check", env=env)
        selection = ["--workspace"] + (["--exclude", "clankerd-guestd"] if system == "Darwin" else [])
        cargo("clippy", "--locked", *selection, "--all-targets", "--", "-D", "warnings", env=env)
    elif operation in ("test", "test-linux"):
        if operation == "test-linux":
            linux_required(system)
        if system == "Linux":
            with linux_test_environment(env) as test_env:
                cargo("test", "--locked", "--workspace", env=prebuild(test_env))
        else:
            bundle = Path(env.get("RUST_OUT", str(ROOT / "build/rust"))).resolve()
            env = dict(env, DYLD_LIBRARY_PATH=str(bundle / "darwin-arm64/lib"))
            print("Running native Mac unit and host-only integration checks; Linux guest suite: make test-linux.", flush=True)
            cargo("test", "--locked", "--workspace", "--exclude", "clankerd-guestd", "--lib", "--bins", env=env)
            for package, tests in (("clankerd-proto", ["spawn"]), ("libkrun-sys", ["errors"]),
                                   ("vmctl", ["version", "smoke_script", "native_bind_semantics"])):
                selection = [arg for name in tests for arg in ("--test", name)]
                cargo("test", "--locked", "-p", package, *selection, env=env)
    else:
        raise RuntimeError(f"Unknown operation: {operation}")


if __name__ == "__main__":
    try:
        if len(sys.argv) != 2:
            raise RuntimeError("Usage: rust-test.py test|test-linux|test-native|lint|coverage|coverage-setup")
        main(sys.argv[1])
    except (RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
