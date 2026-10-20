# Native VM boot and acceptance

Build the native artifact and run the native gates on Apple Silicon
macOS 15:

```bash
mise exec -- make -j4 build
mise exec -- bash scripts/accept-native-build.sh
```

The artifact supplies signed clankerd host executables and
`linux-arm64/management`, a packaged Linux management filesystem. Host libraries
must be installed and resolvable by the native linker and dyld. The toolchain
and Apple's SDK are build prerequisites.

The boot gate exercises workload exit codes and console output, image defaults
and execution overrides, cgroup delegation, stop/kill behavior, cached creation,
ext4 volumes, persistence, bind mounts, DHCP, DNS, HTTPS and attachment cleanup.
Workload completion leaves guest management running until the machine is stopped.
The recorded network PID belongs to one machine; other machines retain their
own attachments when that machine is removed.

Run only the boot gate, including common preparation and management checks:

```bash
mise exec -- bash scripts/accept-native-build.sh m2-boot-check
```

Logs, input identities and a compact result stay under `build/acceptance-<UTC>`.
The entrypoint prints numbered progress and long-running estimates. Use
`ACCEPTANCE_VERBOSE=1` for full terminal output, or summarize a completed run:

```bash
mise exec -- python3 scripts/summarize-native-build.py
```

A native hypervisor entitlement error requires rebuilding the signed artifact
with `make build`. Missing installed libraries produce native linker or loader
errors; resolve the system prerequisite before rerunning acceptance.

See [the build contract](native-build.md), [virtual networks](virtual-networks.md),
and [Rust testing](rust-testing.md).
Host sleep/wake, independent LAN peers, and external IPv6 require separate
observations; a passing gate does not establish those scenarios.
