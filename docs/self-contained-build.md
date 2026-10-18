# Self-contained artifact

Run `mise install`, then `mise exec -- make -j4 build`. Python 3.12+ is a build
prerequisite. Apple Silicon macOS builds require Apple's command-line SDK;
Linux ARM64 and x86_64 cross-builds use the repository's SDK link declarations
and Zig. Cargo owns all clankerd Rust code. The only C compilation is clankerd's
own Apple vmnet transport, checked against Apple's headers on native builds.

`build-support/inputs.mk` locks the guest AArch64/musl distribution, macOS 15
ARM64 bottles, their SHA256 hashes and licenses, and accompanying license texts.
The Alpine minirootfs supplies the base distribution closure; additional APKs
supply packaged dhcpcd and e2fsprogs. No source builds or downstream patches run.
The assembly reproduces dhcpcd's account provisioning and retains its chroot,
loader, libraries, configuration and state directories. APK package metadata and
provisioning scripts accompany the licenses. The minirootfs already contains
BusyBox's applet links. Distribution scripts never execute on the build host.

Make exposes these outputs under `BUILD_ROOT/<execution OS>-<execution arch>`:

| Output | Prerequisites and responsibility |
| --- | --- |
| `downloads/<input>-<sha>.archive` | Locked URL; full hash verified on every invocation |
| `packages/<input>/.complete` | Verified binary extraction, paths and permissions |
| `management/.complete` | Guest packages and explicit provisioning recipe |
| `host-libraries/.complete` | Host bottles, Mach-O import closure and relocation |
| `host-rust/.complete` | Cargo host compilation, host sources and build configuration |
| `guest-rust/.complete` | Cargo guest compilation, guest/protocol sources and build configuration |
| `artifact/.complete` | Completed phases, provenance, licenses, final signing and import audit |

Each phase owns its output and publishes a private generation only after its
recipe succeeds. Content inventories detect missing or damaged files, modes and
symlinks. Cross-process locks serialize publication. Failed steps preserve the
previous artifact; abandoned private directories have no public completion
record. `build/rust` points to a completed immutable generation; `RUST_OUT` changes
that publication path. Copy its **contents** to relocate it. `BUILD_ROOT` and
`CARGO_TARGET_DIR` select isolated build/cache directories. Corrupt cached
archives fail closed; remove the named archive to retry the download. Old
complete generations remain available until `make clean`.

Guestd remains PID1 in management. Workloads and execs join a persistent private
mount namespace rooted in the workload disk, sharing PID and network namespaces.
Their resolver directory is shared deliberately; workload accounts, loaders and
DHCP configuration do not affect management. This filesystem split is not a
containment boundary against privileged root. Disk, volume and bind formats and
lifetime policies are unchanged.

The artifact includes `darwin-arm64/{vmctl,clankerd-vmspawn,clankerd-network}`, all
host dylibs in `darwin-arm64/lib`, and guestd plus `linux-arm64/management`.
Imports resolve through relative Mach-O paths; bundled firmware is preloaded
before libkrun's basename lookup. All native code is signed after relocation.

`clankerd-network` calls the macOS 15 vmnet API directly and exchanges Ethernet
frames through libkrun's Unix datagram attachment. Existing relay source
validation, deterministic prefixes, membership and host PF enforcement remain
in place. Ordinary sudo authentication authorizes a temporary root session in
`/var/run`; it only launches an immutable, root-owned temporary snapshot of
this bundle's native networking/PF executable. The snapshot lives in a private
`/tmp` directory and is deleted with the session; later bundle edits cannot
change already-authorized code.
Native transport relinquishes root after interface creation. The VM is always
unprivileged. The session permits attachment recovery after sudo's credential
cache expires, and exits 30 seconds after its last operation/attachment ends.
No system executable, sudoers rule, shell profile or global environment variable
is installed. Runtime PF state is owned under `/var/run/clankerd-network-pf`;
only the `com.apple/clankerd-networks` anchor and acquired PF reference are managed.

# Verification

`mise exec -- make test` runs graph behavior tests and the Rust suite. Linux
privilege-gated paths may skip when the container lacks mount capabilities.
`mise exec -- make lint` runs formatting and clippy. The graph suite uses real
Make, extraction and assembly with compiler/signing fixtures at tool boundaries.
It covers warm reuse, source and declaration changes, missing outputs, corrupt
inputs, an incorrect hash, failed signing, interruption and parallel invocations.

Run the integrated acceptance on the macOS 15 Apple Silicon host:

```sh
mise exec -- bash scripts/accept-self-contained-build.sh
```

The script builds, verifies, copies and runs the artifact without dependency
path overrides. It validates signatures and every library import, compiles the
native transport against Apple's SDK, tests declined private-path authorization,
and requests ordinary administrator authentication. It records actual loaded
library mappings and VM UIDs, shell-free/conflicting glibc workloads, packaged
DHCP worker privileges/seccomp, mount separation and resolver publication. It
then runs the existing boot/image/volume/bind, command, deterministic-prefix and
PF membership, lifetime, recovery and Docker/Compose/data-preservation gates.
The existing lifetime fixture changes network traffic, not guest management.
Docker input identities are printed in its retained gate log.

Evidence is retained under `build/acceptance-<UTC timestamp>` by default. Set
`EVIDENCE_DIR` to choose the directory and `IMAGE` to select the Docker workload
image. The default Docker workload image must already be available from its
registry, as with the existing acceptance suite. Selected gate names may be
passed after the script name; build and management checks always run. Python,
Cargo, the SDK and diagnostic tools are build/test prerequisites, not runtime
artifact dependencies.

Linux cross-builds establish assembly, linking and signing, not Apple API
behavior. Native authorization, packet transport, VM boot and the integrated
acceptance matrix remain unverified until this script runs on the Mac. Existing
experiment evidence is not acceptance of this implementation. Host sleep/wake
and external IPv6 remain unverified unless actually exercised.

Native gate output is streamed to the terminal and saved in each gate's log.
Quiet gates report elapsed time every 30 seconds. Each gate has a 30-minute
deadline; set `ACCEPTANCE_GATE_TIMEOUT` (seconds) to change it. A timeout sends
TERM to the gate's process group to allow its cleanup trap to run, then KILL
after a 30-second cleanup grace period, and records a failed gate.
