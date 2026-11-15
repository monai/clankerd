# Execution and evidence

## Linux launch and checks

Configure and run existing developer targets.

```sh
mise exec -- cmake --preset default
mise exec -- cmake --build --preset default --target test lint
```

Use `test-build` for Python build contracts and `test-linux` for the Linux Rust workspace independently. `coverage` is the extended instrumented run. Coverage installs a separate pinned nightly toolchain and writes `build/coverage/coverage.json` and `build/coverage/html/index.html`.

Linux needs liblzma development/linker files and e2fsprogs. `CLANKERD_TEST_TOOLS`, when set, must contain executable filesystem tools compatible with the test machine. Privileged ext4 tests require loop devices and mount permissions as well as root. Keep temporary files and build output on the container root filesystem when the workspace is shared.

Guest integration tests execute a local Linux guestd process. They are useful process/protocol checks. They do not boot a native VM. Do not run Docker inside this development container to imitate native clankerd behavior.

## macOS launch and doctor

The operator needs native Apple Silicon macOS, Python 3.12+, Apple's command-line SDK, libkrun, libkrunfw and liblzma. See the build and helper setup in `README.md`. Configure `default` for existing native library paths or `homebrew` for Homebrew discovery. Build with the same preset used to configure.

```sh
mise exec -- cmake --preset default
mise exec -- cmake --build --preset default --target build
vmctl=build/artifact/darwin-arm64/vmctl
"$vmctl" --version
"$vmctl" --describe-contract
```

Keep `darwin-arm64/` and `linux-arm64/management/` together. Retain the source revision, dirty patch and build output. Version alone does not identify a dirty build. The read-only doctor is version plus contract metadata from that exact executable. Once isolated state is selected, `machine ls --all --output json` checks that it can be read before driving it.

`CLANKERD_STATE_DIR`, `CLANKERD_RUNTIME_DIR` and `CLANKERD_CACHE_DIR` select independent state, sockets and cache. Use short temporary runtime paths for Unix sockets. Native subnet allocation checks the selected state directory; separate state directories do not prevent native network conflicts. Installing or restarting the system network helper requires administrator authorization. The VM runs as the operator's user.

## Network helper

After building with the selected preset, install the matching helper with
`install-network-helper`; stop development VMs first. `verify-network-helper`
checks packet descriptors, gateway ARP, owner disconnect and stale-generation
safety on 192.168.231.0/24. Ensure that subnet is unused by unrelated vmnet networks.
It does not establish guest connectivity, PF isolation, Shared Internet or bridging.

Helper-failure probes require an idle or isolated host because helper termination
interrupts all clients. Check saved PF evidence after failed release and confirm
startup refuses requests until recovery succeeds. Existing machines need stop/start;
restarting the helper alone does not reconnect them. No stable executable recovery
journey exists yet.

## Drive

The prototype lifecycle scaffold is `crates/vmctl/tests/native_verification.rs`. Its stable target builds the signed bundle and runs the ignored Rust journey directly through Cargo.

```sh
mise exec -- cmake --build --preset default --target verify-host-lifecycle
```

CMake supplies `OUT` from its configured signed artifact directory. `IMAGE` selects the fixture image, defaulting to `ghcr.io/monai/clankers:slim`. The Rust journey checks exec after workload completion and disk persistence across stop/start, then asserts owned-machine removal. Each CLI command has a 180-second deadline through `test-support`; stdout and stderr files exist while it runs. Timeout terminates the command process group, but does not itself establish VM removal. Cleanup uses bounded public CLI commands. The printed `/tmp/clankerd-verification-*` directory contains `manifest.json`, `source.patch`, numbered command/result/stdout/stderr files, console output and `cleanup.json`. The manifest records checkout revision, tracked patch digest and binary digest separately. It does not establish that binary bytes were built from that checkout. Untracked files are not included in the patch identity. Return this directory and the Cargo exit status to the Linux agent. Native VM execution remains unverified here.

Storage, networking, recovery, workloads, distribution and bridge groups remain planned coverage, without executable targets. Terminal, stdin, stream separation and signal assertions remain planned within lifecycle coverage. The existing `test-native` target builds signed artifacts and runs ignored native bind-semantic tests serially. It is a storage subset.
```sh
mise exec -- cmake --build --preset default --target test-native
```

Interactive PTY observations require a terminal or maintained PTY fixture. A noninteractive stdout assertion does not establish terminal resize or signal behavior. Networking checks need actual peer services. Shared Internet and bridged LAN checks must state external connectivity prerequisites. Docker/Compose verification runs only inside a real clankerd VM using `images/dind/README.md`.

## Evidence and cleanup

Retain source and artifact identity, selected groups, prerequisites, command argv, exit status, stdout, stderr, before/after resource inspection and relevant console logs. For file promises, retain observed content and metadata. For reachability promises, retain client results and server observations. Keep evidence outside removable state/cache/runtime directories.

Remove only named machines, networks and volumes created by the journey through the public API. Stop or force-remove owned machines before removing their networks and volumes. Collect logs before machine removal. Explicit volume removal destroys its data. Check cleanup results; deleting a scratch directory alone does not establish that native processes or network attachments ended. Leave unrelated machines and host firewall settings untouched.

After cleanup, verify evidence still exists. Report native groups as unverified until the operator returns actual execution results. This draft has no measured runtime budgets and makes no claim that planned host assertions passed.
