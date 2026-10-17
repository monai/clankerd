#!/usr/bin/env python3
"""Build and publish a complete signed Mac host / Linux guest bundle."""
import contextlib
import fcntl
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
DARWIN = "aarch64-apple-darwin"
GUEST = "aarch64-unknown-linux-musl"


def run(args, *, env=None, capture=False, check=True):
    print("+ " + " ".join(map(str, args)), flush=True)
    return subprocess.run(args, cwd=ROOT, env=env, check=check, text=True,
                          stdout=subprocess.PIPE if capture else None)


def require(*tools):
    missing = [tool for tool in tools if shutil.which(tool) is None]
    if missing:
        raise RuntimeError("Missing build prerequisites: " + ", ".join(missing) +
                           ". Install platform tools and run mise install.")


def environment(resign=False):
    system, arch = platform.system(), platform.machine()
    if system not in ("Darwin", "Linux") or arch not in ("arm64", "aarch64", "x86_64"):
        raise RuntimeError(f"Unsupported build execution platform: {system}/{arch}")
    if system == "Darwin" and arch != "arm64":
        raise RuntimeError("Native Mac builds require Apple Silicon")
    require("rcodesign")
    if not resign:
        require("cargo", "zig", "tar", "patch", "make", "cc")
    env = os.environ.copy()
    jobs = env.get("BUILD_JOBS") or env.get("CARGO_BUILD_JOBS") or str(os.cpu_count() or 1)
    if not jobs.isdecimal() or int(jobs) < 1:
        raise RuntimeError("BUILD_JOBS must be a positive integer")
    env["BUILD_JOBS"] = jobs
    env["CARGO_BUILD_JOBS"] = jobs
    if system == "Darwin" and not resign:
        require("xcrun")
        env["SDKROOT"] = run(["xcrun", "--sdk", "macosx", "--show-sdk-path"], capture=True).stdout.strip()
        if not Path("/opt/homebrew/lib/libkrun.dylib").exists():
            raise RuntimeError("Missing Homebrew libkrun: brew install libkrun")
    return system, env


def build_executables(args, env):
    process = run(["cargo", *args, "--locked", "--message-format=json"],
                  env=env, capture=True, check=False)
    result = {}
    for line in process.stdout.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            print(line)
            continue
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            result[message["target"]["name"]] = Path(message["executable"])
        elif message.get("reason") == "compiler-message":
            print(message["message"].get("rendered", ""), file=sys.stderr, end="")
    process.check_returncode()
    return result


def sign(bundle, env):
    run(["rcodesign", "sign", "-e", str(ROOT / "crates/clankerd-vmspawn/entitlements.plist"),
         "--binary-identifier", "io.clankerd.vmspawn",
         str(bundle / "darwin-arm64/clankerd-vmspawn")], env=env)
    run(["rcodesign", "sign", "--binary-identifier", "io.clankerd.vmctl",
         str(bundle / "darwin-arm64/vmctl")], env=env)


@contextlib.contextmanager
def output_lock(output):
    output.parent.mkdir(parents=True, exist_ok=True)
    with (output.parent / f".{output.name}.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def publish(stage, output, generations):
    # Legacy builds used a real directory; retain it on the first migration.
    # Later publications replace only a symlink, atomically for readers.
    legacy = None
    link = stage.parent / (stage.name + ".link")
    try:
        link.symlink_to(os.path.relpath(stage, output.parent), target_is_directory=True)
        if output.exists() and not output.is_symlink():
            legacy = Path(tempfile.mkdtemp(prefix="legacy-", dir=generations))
            legacy.rmdir()
            output.rename(legacy)
        try:
            os.replace(link, output)
        except BaseException:
            if legacy is not None:
                legacy.rename(output)
            raise
    finally:
        link.unlink(missing_ok=True)


def build(output, resign=False):
    system, env = environment(resign)
    generations = output.parent / f".{output.name}-generations"
    with output_lock(output):
        if output.exists() and not output.is_symlink():
            if not output.is_dir() or any(
                path.name not in {"darwin-arm64", "linux-arm64"} for path in output.iterdir()
            ):
                raise RuntimeError(f"Output is not a build bundle: {output}")
        generations.mkdir(exist_ok=True)
        stage = Path(tempfile.mkdtemp(prefix="bundle-", dir=generations))
        try:
            if resign:
                if not output.is_dir():
                    raise RuntimeError("No completed build to sign; run make build first")
                shutil.copytree(output, stage, dirs_exist_ok=True)
            else:
                host_env = env.copy()
                if system == "Linux":
                    host_env["SDKROOT"] = str(ROOT / "scripts/macos-sdk-stubs")
                host = build_executables(
                    ["build" if system == "Darwin" else "zigbuild", "--release", "--target", DARWIN,
                     "-p", "vmctl", "-p", "clankerd-vmspawn"], host_env)
                guest = build_executables(
                    ["build", "--release", "--target", GUEST, "--config",
                     f'target.{GUEST}.linker="rust-lld"', "-p", "clankerd-guestd"], env)
                for directory, binaries, names in [
                    ("darwin-arm64", host, ["vmctl", "clankerd-vmspawn"]),
                    ("linux-arm64", guest, ["clankerd-guestd"]),
                ]:
                    (stage / directory).mkdir()
                    for name in names:
                        shutil.copy2(binaries[name], stage / directory / name)
                for script in ["build-e2fsprogs.sh", "build-dhcpcd.sh"]:
                    run([str(ROOT / "scripts" / script), str(stage / "linux-arm64")], env=env)
            sign(stage, env)
            publish(stage, output, generations)
        except BaseException:
            shutil.rmtree(stage)
            raise
    print(f"Completed signed build: {output}")


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in ("build", "sign"):
        raise RuntimeError("Usage: rust-build.py build|sign")
    output = Path(os.environ.get("RUST_OUT", "build/rust"))
    if not output.is_absolute():
        output = ROOT / output
    # Do not resolve the final symlink: publication must replace the public name.
    output = output.parent.resolve() / output.name
    build(output, resign=sys.argv[1] == "sign")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError, OSError, KeyError) as error:
        sys.exit(str(error))
