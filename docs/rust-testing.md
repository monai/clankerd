# Rust tests and coverage

`mise exec -- make test lint` runs the workspace tests and lint checks
with the pinned production compiler; the selected suite depends on the host.

Run coverage on Linux. It uses [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), pinned
in `mise.toml`. It measures LLVM line, region, function and branch coverage.
[LLVM continuous profiles](https://clang.llvm.org/docs/SourceBasedCodeCoverage.html)
retain counters when tests kill helper processes;
this is compiler instrumentation, with no profiling code in production sources.
Branch instrumentation requires nightly Rust; `build-support/checks.mk` pins a
separate coverage toolchain without changing the production compiler.

```sh
mise install
mise exec -- make coverage-setup coverage
```

Reports are `build/coverage/coverage.json` and `build/coverage/html/index.html`.
CI generates and uploads both. Override `COVERAGE_OUT` for report placement and
`CARGO_TARGET_DIR` for build placement. In a container with a shared workspace,
keep the build directory and `TMPDIR` on the container's root filesystem.

`CLANKERD_TEST_TOOLS` can point at compatible mke2fs, e2fsck, debugfs and
resize2fs binaries. Tests obtain helper executable paths from Cargo's JSON
build messages, so instrumented binaries are included without assuming Cargo's
output layout.

Unprivileged coverage does not exercise root-only ownership and mounted ext4
population tests, native macOS hypervisor paths, or opt-in registry tests.
Uncovered Linux paths remain visible; macOS-only code is absent from this
report. Coverage does not replace the existing privileged seam B checks or
Mac smoke tests.

`vmctl/tests/native_bind_semantics.rs` contains opt-in tests of actual Mac
virtio-fs ownership, permission side effects and host read-only enforcement.
`mise exec -- make test-native` builds signed native helpers and runs these checks
on Apple Silicon. These tests are ignored in the ordinary workspace suite;
Linux compilation or schema/protocol tests do not establish native behavior.

A loop device presents a regular disk-image file as a Linux block device. The
population test uses `/dev/loop-control` to attach an image to `/dev/loopN`,
then mounts its ext4 filesystem and unpacks the image. It requires root,
loop devices and mount permissions; uid 0 alone is insufficient in a restricted
container. [Linux loop-device documentation](https://man7.org/linux/man-pages/man4/loop.4.html).

`build-support/checks.mk` declares developer commands separately from artifact
production. `make test` runs Python contract tests and direct Cargo tests; it
does not build the distributable bundle. `make test-scripts` and `make test-rust`
select either suite independently. On Linux, Cargo runs the workspace suite.
Test fixtures build their executable helpers through Cargo when needed. On macOS
the test graph installs and signs the pinned host dylibs, then links test
executables with a test-only rpath to that library generation. No guest image,
product executable, or published bundle is required for ordinary Mac tests.
The Mac suite runs host unit tests, protocol/libkrun integration tests, and CLI version
and smoke-script checks. Guest integration tests execute a local Linux guest
process and therefore require Linux; run `mise exec -- make test-linux` in a
Linux context. Mac lint checks all host targets and excludes the Linux-only
guest package. Lint invokes `cargo fmt` and `cargo clippy` directly.

Coverage alone uses `scripts/rust-coverage.py` to pass `CLANKERD_TEST_BINARIES`,
a JSON map of helper names to Cargo-reported executable paths. Ordinary Cargo
tests use one cached, locked helper build per executable per test process.
Coverage instruments the helper prebuild and the test run with the
same llvm-cov environment, and preserves `CARGO_LLVM_COV_TARGET_DIR` across
collection and every report.

Linux tests require native e2fsprogs (`apt-get install e2fsprogs` on Debian/Ubuntu).
On ARM64 Linux, Make supplies tools from the packaged management filesystem;
other Linux hosts use their native tools. The coverage runner validates tool
availability before compilation. `CLANKERD_TEST_TOOLS` is authoritative when set;
it must contain executable tools
for the test machine, which may differ from the shipped ARM64 guest target.
