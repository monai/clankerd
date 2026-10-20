# clankerd

A Rust library and CLI for running Linux machines from OCI images on Apple
Silicon Macs, using libkrun and the stock libkrunfw kernel.

| Component | Purpose |
| --- | --- |
| `libclankerd` | Machine lifecycle, images, volumes, exec and forwarding APIs |
| `vmctl` | Docker-style machine CLI |
| `clankerd-vmspawn` | Signed VM helper that owns forwarding and outlives the CLI |
| `clankerd-guestd` | Guest PID 1: boot setup, workloads, exec and shutdown |
| `clankerd-proto` | Shared host/guest protocol and stream codecs |
| `libkrun-sys` | libkrun FFI |

Workloads use the image as their root filesystem, independently of guest management. Named volumes persist across
restarts and image changes, host directories are shared through virtio-fs, and
host Unix sockets can be exposed through management relays. Images with Docker and nftables can run containers and
Compose inside the machine.

## Build

Install the pinned tools with mise, then build and sign the binaries:

```sh
mise install
mise exec -- make build
```

Python 3.12+, Apple's command-line SDK and installed host libraries are
required on Apple Silicon macOS. Host executables build only on native macOS.
Linux supports guest builds and developer tests.

The native linker resolves libkrun and liblzma; dyld resolves their runtime
dependencies, including libkrunfw. Installed libraries must export the APIs used
by the application. The build assumes no installation prefix or package manager
and adds no library search paths. Missing libraries produce native linker or
loader errors. liblzma source compilation is disabled for supported host targets.

`Makefile` downloads verified guest inputs, assembles management, runs Cargo,
signs clankerd's executables and atomically publishes `build/artifact` as a
symlink to a complete immutable bundle. It contains host executables under
`darwin-arm64` and guestd plus the Alpine management filesystem under
`linux-arm64/management`, with guest package metadata and license texts.
Guest versions and checksums are locked in `build-support/inputs.mk`.
`RUST_OUT` selects the publication path, `BUILD_ROOT` selects Make intermediates,
and `CARGO_TARGET_DIR` selects Cargo intermediates. Cargo owns compilation
freshness and Make tracks installed file prerequisites.

Host networking uses bundled clankerd-owned Apple vmnet and PF code.
Administrator authorization uses ordinary sudo authentication. The VM and
Ethernet transport remain unprivileged; no helper installation, sudoers edits,
or persistent configuration changes are needed.

Run all Mac build/artifact checks and native gates with:

```sh
mise exec -- bash scripts/accept-native-build.sh
```

See [native build](docs/native-build.md) for prerequisites and validation.

## Run a machine

```sh
vmctl=build/artifact/darwin-arm64/vmctl
"$vmctl" machine create --name sandbox debian:trixie
"$vmctl" machine start sandbox
"$vmctl" machine exec -it sandbox /bin/bash
"$vmctl" machine stop sandbox
"$vmctl" machine rm sandbox
```

Guestd is the machine's init process and stays running until `stop` or `kill`.
The image's ENTRYPOINT/CMD runs once per start; its exit does not stop the VM.
`run` returns the command's exit code while leaving the machine running;
`run -it` attaches to that command. `exec -it` opens a separate session even
when the image command has already finished. Restart policies apply to machine
failure, not to an image command exiting. Removing a machine retains named volumes; delete data explicitly with `vmctl volume rm NAME`.

Run `vmctl --help` for lifecycle commands and `vmctl run --help` for image,
resource, mount, socket and environment flags.
Global options precede the resource or shortcut; action options precede operands.
Lists default to tables; use `--output json` before operands for structured
results. `vmctl --describe-contract` exports deterministic command metadata and
JSON schemas for the public result records.

Mac bind mounts default to **simplified** permission semantics: files appear
owned by the requesting guest user, `chown` does not persist ownership, and
`chmod` changes the actual Mac permission bits. To preserve guest ownership and
permissions in host extended attributes, select **complete** per attachment:

```sh
vmctl create --mount type=bind,source=./src,target=/src,semantics=complete IMAGE
vmctl update --mount-remove /src --mount-add type=bind,source=./src,target=/src,semantics=complete MACHINE
```

In the library, use
`Mount::bind(source, target).with_semantics(BindSemantics::Complete)`.
Omitting `semantics`, including in saved configuration, always means simplified.
The choice is independent of image `USER` and run/exec user overrides. It applies
only to host bind mounts; machine disks and named volumes retain native Linux
ownership. Read-only bind mounts are enforced by both libkrun and the guest mount.
Installed libkrun must provide the virtio-fs API used by these attachments.

Stopped-machine `update --mount-remove TARGET` and `--socket-remove TARGET`
ensure the attachment is absent, succeeding when it was already absent.
Removal preserves stored data; missing or running machines still fail.
Application leases, mDNS and managed Chrome are outside the current runtime's API.

## Test

Run the Linux workspace tests and strict formatting/lint checks:

```sh
mise exec -- make test lint
```

Generate line and branch coverage using the separately pinned nightly compiler:

```sh
mise exec -- make coverage-setup coverage
```

Reports are written to `build/coverage/`. Privileged filesystem tests need root,
loop devices and mount permissions. See [Rust testing](docs/rust-testing.md) for
coverage setup and container build directories.

[Docker inside a machine](docs/docker-in-machine.md) describes image setup and
the Mac smoke scripts. Docker image assets live in `images/slim/`.

CI tests the Rust workspace, runs privileged filesystem checks, generates
coverage, and builds and signs the host/guest binaries. Tagged releases package
those binaries and guest utilities.

Named-volume mounts form a collection: for example,
`vmctl create --name app -v data:/data -v logs:/logs IMAGE`. Each volume can appear once per machine;
guest targets must be unique across bind and volume mounts. Volumes are attached
in mount order after the optional image root disk. Detach, reimage and machine
removal retain their data. Use matching versions of vmctl and clankerd-vmspawn;
the helper protocol now writes a `volume_disks` collection (the new helper also
accepts older singular `volume_disk` specs). Native disk counts remain subject
to [libkrun's shared device/IRQ capacity](https://github.com/containers/libkrun/blob/main/src/libkrun/src/vmm/device_manager/hvf/mmio.rs);
attachment failures are reported at boot.
