# Rust tests and coverage

`mise exec -- make test lint` runs the workspace tests and lint checks
with the pinned production compiler.

Run coverage on Linux. It uses [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), pinned
in `mise.toml`. It measures LLVM line, region, function and branch coverage.
[LLVM continuous profiles](https://clang.llvm.org/docs/SourceBasedCodeCoverage.html)
retain counters when tests kill helper processes;
this is compiler instrumentation, with no profiling code in production sources.
Branch instrumentation requires nightly Rust; `Makefile` pins a separate
coverage toolchain without changing the production compiler.

```sh
mise install
mise exec -- make coverage-setup coverage
```

Reports are `build/coverage/coverage.json` and `build/coverage/html/index.html`.
CI generates and uploads both. Override `COVERAGE_OUT` for report placement and
`CARGO_TARGET_DIR` for build placement. In a container with a shared workspace,
keep the build directory and `TMPDIR` on the container's root filesystem.

`CLANKERD_TEST_BOOT_DIR` can point at compatible mke2fs, e2fsck, debugfs and
resize2fs binaries. Tests obtain helper executable paths from Cargo's JSON
build messages, so instrumented binaries are included without assuming Cargo's
output layout.

Unprivileged coverage does not exercise root-only ownership and mounted ext4
population tests, native macOS hypervisor paths, or opt-in registry tests.
Uncovered Linux paths remain visible; macOS-only code is absent from this
report. Coverage does not replace the existing privileged seam B checks or
Mac smoke tests.

A loop device presents a regular disk-image file as a Linux block device. The
population test uses `/dev/loop-control` to attach an image to `/dev/loopN`,
then mounts its ext4 filesystem and unpacks the image. It requires root,
loop devices and mount permissions; uid 0 alone is insufficient in a restricted
container. [Linux loop-device documentation](https://man7.org/linux/man-pages/man4/loop.4.html).
