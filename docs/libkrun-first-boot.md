# Native VM boot and acceptance

Build the self-contained artifact and run the native gates on Apple Silicon
macOS 15:

```bash
mise exec -- make -j4 build
mise exec -- bash scripts/accept-self-contained-build.sh
```

The artifact supplies the signed host executables and dynamic libraries plus
`linux-arm64/management`, a packaged Linux management filesystem. No Homebrew
runtime dependencies, third-party helper installation or sudoers edits are
required. The toolchain and Apple's SDK are build prerequisites.

The boot gate exercises workload exit codes and console output, image defaults
and execution overrides, cgroup delegation, stop/kill behavior, cached creation,
ext4 volumes, persistence, bind mounts, DHCP, DNS, HTTPS and attachment cleanup.
Workload completion leaves guest management running until the machine is stopped.
The recorded network PID belongs to one machine; other machines retain their
own attachments when that machine is removed.

Run only the boot gate, including common preparation and management checks:

```bash
mise exec -- bash scripts/accept-self-contained-build.sh m2-boot-check
```

Logs, input identities and a compact result stay under `build/acceptance-<UTC>`.
The entrypoint prints numbered progress and long-running estimates. Use
`ACCEPTANCE_VERBOSE=1` for full terminal output, or summarize a completed run:

```bash
mise exec -- python3 scripts/summarize-self-contained-build.py
```

A native hypervisor entitlement error requires rebuilding the signed artifact
with `make build`. Missing bundled libraries likewise require fixing the
artifact build rather than installing host runtime libraries.

See [the build contract](self-contained-build.md), [virtual networks](virtual-networks.md),
[Rust testing](rust-testing.md), and [review evidence](self-contained-build-review.md).
Host sleep/wake, independent LAN peers, and external IPv6 require separate
observations; a passing gate does not establish those scenarios.
