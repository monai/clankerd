# Native VM boot verification

Build and run the implemented Rust journeys on Apple Silicon macOS:

```sh
mise exec -- cmake --preset default
mise exec -- cmake --build --preset default --target test-native
mise exec -- cmake --build --preset default --target verify-host-lifecycle
```

Use the `homebrew` preset for Homebrew library discovery, with the same preset for configuration and builds. Keep the signed host executables and `linux-arm64/management` together. Installed host libraries must remain resolvable by the native linker and dyld.

The lifecycle journey checks exec after workload completion, root-disk persistence across stop/start and owned-machine removal. The native bind suite checks bind semantics. The retained evidence directory identifies the checkout and binary independently. See the [verification skill](../.agents/skills/verify-clankerd/SKILL.md) for the operator handoff.

Networking, recovery, Docker workloads, distribution relocation, host sleep/wake and external IPv6 require separate native evidence. These checks remain planned; the implemented journeys do not establish them.

A hypervisor entitlement error requires rebuilding the signed artifact. Missing installed libraries require resolving the host prerequisite before retrying. See [native build](native-build.md), [virtual networks](virtual-networks.md), and [Rust testing](rust-testing.md).
