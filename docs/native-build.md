# Native build

Build host executables on Apple Silicon macOS with the tools in `mise.toml`,
Python 3.12+ and Apple's command-line SDK. Install libkrun, its runtime
dependencies (including libkrunfw), and liblzma through your system's normal
facilities before building. No package manager or installation prefix is assumed.
The native linker and dyld must be able to resolve them. The core build neither
checks library release versions nor discovers library search paths. Missing symbols,
libraries or incompatible ABIs fail through the native toolchain or loader.
Cargo's liblzma build-script overrides require installed libraries and suppress
source compilation on macOS ARM64 and Linux ARM64/x86_64 GNU hosts.

```sh
mise exec -- cmake --preset default
mise exec -- cmake --build --preset default --target build
```

For libraries installed through Homebrew, use the optional entrypoint:

```sh
mise exec -- cmake --preset homebrew
mise exec -- cmake --build --preset homebrew --target build
```

It queries `brew --prefix` for libkrun, libkrunfw, xz and libkrun's installed
dependencies. It supplies native linker directories and embeds absolute runtime
search paths to these installed libraries in host binaries. No third-party
libraries are installed, copied, rewritten or signed. Guest compilation receives
no Homebrew flags. This delegates to the same `build` graph and publication.
The resulting host binaries still require those installed libraries, but should
run without `DYLD_*` variables. Moving them to another machine requires libraries
resolvable through the encoded paths or rebuilding for that installation.

The package-manager-independent `cmake --build --preset default --target build` continues to use the caller's
native toolchain configuration; it never invokes Homebrew. Host flag overrides
in CMake use Cargo's encoded argument format so library paths remain
single arguments. Cargo owns recompilation when those flags change.

CMake owns checksum-verified guest downloads, assembly dependencies, executable
signing and atomic publication. Corrosion invokes Cargo for Rust compilation.
Cargo also keeps ownership of crate-local C compilation and native linking.
The two retained Python build scripts implement archive safety and guest
filesystem policy. They preserve package metadata, symlinks, overlay order,
permissions and architecture checks. These operations remain in Python because
they require product-specific traversal and validation. The small shell publication
script keeps signal cleanup and atomic symlink replacement together. `build/artifact`
points to an immutable complete bundle in `build/artifact.bundles/`. Failed
builds leave the published bundle intact. A warm build reuses unchanged files.
The artifact includes clankerd's executables, Alpine guest management and guest
license metadata. Running it on another Mac requires installed host libraries
that its native loader can resolve.

Run the implemented native Rust checks after configuring the selected preset:

```sh
mise exec -- cmake --build --preset default --target test-native
mise exec -- cmake --build --preset default --target verify-host-lifecycle
```

`test-native` checks bind semantics. `verify-host-lifecycle` checks exec after workload completion, disk persistence and owned-machine cleanup. Neither establishes distribution relocation, networking recovery or Docker workload behavior. Those journeys remain planned in the [verification skill](../.agents/skills/verify-clankerd/SKILL.md).

Linux developer checks require installed liblzma development/linker files and
e2fsprogs; guest compilation remains available with `cmake --build --preset default --target cargo-guest`.
Linux checks do not establish native Mac linkage, loader or VM behavior.

## Network LaunchDaemon prototype

Native networking requires the system-wide `io.clankerd.network` LaunchDaemon.
The CLI and VM runner remain ordinary user processes. Install from this checkout
on macOS (use `homebrew` throughout instead of `default` when applicable):

```sh
mise exec -- cmake --preset default
mise exec -- cmake --build --preset default --target install-network-helper
mise exec -- cmake --build --preset default --target verify-network-helper
mise exec -- cmake --build --preset default --target verify-network-relay
mise exec -- cmake --build --preset default --target status-network-helper
mise exec -- cmake --build --preset default --target verify-host-lifecycle
```

`install-network-helper` builds the signed artifact, asks for administrator access,
replaces `/Library/PrivilegedHelperTools/clankerd-network` and installs
`/Library/LaunchDaemons/io.clankerd.network.plist`. It registers the daemon with
launchd; the first XPC request starts it. Run this target again after helper changes.
The client and helper must use matching builds: native XPC messages replace the
older JSON envelope.
Normal builds do not install system files. Stop development VMs before reinstalling.
There is no binary comparison or automatic idle shutdown.

`verify-network-helper` requires the installed helper. It creates two host-only
interfaces, obtains packet socket descriptors, checks generation handling and
checks that dropping one owner connection releases only its interface. It uses
192.168.231.0/24; run without an unrelated vmnet network on that subnet. This test
does not prove guest traffic, shared Internet access, PF isolation or bridging.
`verify-network-relay` checks bidirectional forwarding with libkrun’s non-terminated
Unix socket address format, including the macOS-specific address handling.

```sh
mise exec -- cmake --build --preset default --target restart-network-helper
mise exec -- cmake --build --preset default --target uninstall-network-helper
```

Restart and uninstall require administrator access. Restarting, crashing or
replacing the daemon breaks networking for all running VMs. Stop/start those VMs
to restore networking. No interface or packet connection is recovered automatically.
The restart target can also start a registered daemon that has not yet launched.
Uninstall stops the service and removes its executable and plist.

The daemon identifies users from XPC credentials, as the old implementation did
through its user-specific authorization sockets. It does not check program code
signatures. Any process running as a user can request that user's network operations.
