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

Machines boot the image as their root filesystem. Named volumes persist across
restarts and image changes, host directories are shared through virtio-fs, and
host Unix sockets can be exposed through management relays. Images with Docker and nftables can run containers and
Compose inside the machine.

## Build

Install the pinned tools with mise, then build, sign and verify the binaries:

```sh
mise install
mise exec -- make build
```

The build runs on Linux or macOS. Outputs are:

- `build/rust/darwin-arm64/{vmctl,clankerd-vmspawn}`: Mac host binaries.
- `build/rust/linux-arm64/`: static guest daemon and ext4 utilities.

The Mac needs Homebrew libkrun/libkrunfw and an installed vmnet-helper.
On macOS 15, install its supported passwordless sudoers rule; the VM stays unprivileged.
See [Mac setup and validation](docs/libkrun-first-boot.md) for prerequisites,
SDK setup, signing and hardware checks. `make rust` remains an alias for the
complete build.

## Run a machine

```sh
vmctl=build/rust/darwin-arm64/vmctl
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
