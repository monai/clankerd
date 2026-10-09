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
published ports bind to host loopback. Host ports and Unix sockets can be exposed
through generic tunnels. Images with Docker and nftables can run containers and
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

The Mac needs Homebrew libkrun/libkrunfw. The runtime can fetch the pinned
gvproxy network sidecar.
See [Mac setup and validation](docs/libkrun-first-boot.md) for prerequisites,
SDK setup, signing and hardware checks. `make rust` remains an alias for the
complete build.

## Run a machine

```sh
vmctl=build/rust/darwin-arm64/vmctl
"$vmctl" run -d --name sandbox ghcr.io/monai/clankers:slim sleep infinity
"$vmctl" exec sandbox -it /bin/bash
"$vmctl" stop sandbox
"$vmctl" rm sandbox
```

Use `run -it IMAGE COMMAND` to attach to the main workload; the machine exits
when that workload exits. `exec -it` opens a separate session in a running
machine. Removing a machine keeps its named volumes unless `rm -v` is requested.

Run `vmctl --help` for lifecycle commands and `vmctl run --help` for image,
resource, mount, port and environment flags. Application leases, mDNS and
managed Chrome are outside the current runtime's API.

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
