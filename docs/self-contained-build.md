# Self-contained artifact

Run `mise install`, then `mise exec -- make -j4 build`. Python 3.12+ is a build
prerequisite. `pkg-config` is a build tool prerequisite for linking the locked
host liblzma; its absence fails configuration rather than selecting a source
build. Apple Silicon macOS builds require Apple's command-line SDK;
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
| `downloads/<input>-<sha>.archive` | Locked URL; checksum verified before download publication and extraction |
| `packages/<input>/.stamp` | Verified binary extraction, paths and permissions |
| `management/.stamp` | Guest packages and explicit provisioning recipe |
| `host-libraries/.stamp` | Host bottles, Mach-O import closure and relocation |
| `cargo/<target>/release/<program>` | Cargo owns source dependencies and compilation; Make consumes the executable outputs |
| `artifact/` inside the work directory | Incremental installation, provenance, licenses, signing and import audit |
| `build/artifact` | Published symlink to a complete, audited bundle under `build/artifact.bundles/` |

Cargo is invoked on every build and decides whether compilation is needed.
Make does not scan Rust sources or maintain source-hash manifests. Unchanged
executables retain their timestamps, so an unchanged Cargo build does not
reinstall or resign the artifact. Installation consumes Cargo outputs directly;
there are no intermediate host/guest Rust staging directories.

Make uses file prerequisites and ordinary timestamp stamps for directory
assembly. Cargo owns compiler freshness. There are no environment fingerprints,
content inventories, recipe receipts, or custom build cache locks. Apple's Make
3.81 misses some subsecond changes, so file recipes use the system `find -newer`
to check prerequisites at nanosecond precision before doing work.
Downloads use curl and shasum; `make verify-inputs` explicitly
rechecks retained archives. A corrupt input fails checksum verification when
extracted; remove the named archive to download it again.

`build/artifact` is a relative symlink. Make installs and signs incrementally in
the work directory's `artifact/` tree and audits it before publication. When
outputs change, Make copies the completed tree to a fresh bundle directory and
atomically renames a temporary symlink over `build/artifact`. A failed assembly,
signature, audit, copy or pre-switch rename leaves the previous bundle published.
The final copy preserves file permissions, timestamps and internal symlinks.
An unchanged warm build neither copies nor switches the published bundle.

Published bundle directories are never updated by subsequent builds. Previous
bundles remain available until `make clean`, including for readers that resolved
the published symlink before a switch. A reader needing a stable tree across
multiple file accesses must resolve the symlink once and use that bundle path.
This is an atomic path switch, not a filesystem transaction across separately
opened paths or a power-loss durability guarantee.

Existing ordinary publication directories and unrelated symlinks are rejected;
remove the old `build/artifact` directory once before using this layout. There is
no automatic migration. Make repairs missing declared staging outputs and
republishes when a declared published file is missing. It does not perform
arbitrary corruption repair. Use `make clean`
when switching SDKs/signing tools or changing the packaging output set. Cargo
handles its own compiler, profile and Rust flag changes. Run independent Make
processes with distinct `BUILD_ROOT`, `DOWNLOADS`, and `RUST_OUT` paths;
`make -j` handles parallel work within one build. `CARGO_TARGET_DIR` selects
Cargo's own build directory. No additional cache decides whether a phase runs.

The workload's `/run` is a separate tmpfs created on each boot. Runtime PID files
and sockets expire at reboot; the image filesystem and data volumes persist.
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
inputs, an incorrect hash, failed signing/audit/publication, interruption before
the publication switch, retention of old bundles and parallel Make recipes.

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
behavior. The complete native matrix passed on Apple Silicon macOS 15.8.1 in
`build/acceptance-20261019T133840Z`, as reported by the user. That run predates
atomic final-bundle publication; the current layout requires a fresh native run. See
[self-contained-build-review.md](self-contained-build-review.md) for the retained
Mac evidence location and covered gates. Host sleep/wake, independent LAN-peer
coverage, and external IPv6 remain unverified unless actually exercised.

Native gate output is saved in each gate's log. The terminal shows compact
numbered progress and gate results; `ACCEPTANCE_VERBOSE=1` also streams full output.
Quiet gates report elapsed time every 30 seconds. Each gate has a 30-minute
deadline; set `ACCEPTANCE_GATE_TIMEOUT` (seconds) to change it. A timeout sends
TERM to the gate's process group to allow its cleanup trap to run, then KILL
after a 30-second cleanup grace period, and records a failed gate.

Progress uses `[n/total]` and marks potentially slow steps with approximate
runtimes. The entrypoint runs a snapshot of its own shell text, so editing its
file during a long gate cannot corrupt the running shell's unread commands.
To rerun only the two gates that failed in the initial native run (plus the
standard preparation/management/bind checks):

```bash
mise exec -- bash scripts/accept-self-contained-build.sh m2-boot-check m2-smoke-test
```

A successful selective run reports success for those selected gates; retain the
previous evidence for the other gates.

For compact output to share after a run, use this single-line command:

```bash
mise exec -- python3 scripts/summarize-self-contained-build.py
```

It reads the latest completed acceptance evidence without rerunning any VMs,
prints gate statuses and up to 20 lines around each failure before cleanup's
verbose log dumps, and saves the same text as `paste-summary.txt` beside the
original logs. An optional evidence-directory argument selects an older run.

Gate timeout isolation uses a separate process group in the authorizing
terminal session, so cached sudo authorization remains available to the
native-prefix gate's noninteractive diagnostic and fixture commands.

## Current artifact and interface contract

`linux-arm64/management` is the sole guest management filesystem. Guestd lives
at `usr/sbin/clankerd-guestd`; distribution tools retain `sbin/` and `usr/sbin/`
paths. Runtime configuration is `etc/clankerd/workload.json`. No executable or
tool alias is placed at the filesystem root, and no standalone guestd copy is
published under `linux-arm64`.

`build-support/management-rootfs/` owns the management account databases,
locked passwords, DHCP policy, and executable network hook. These checked-in
files overlay distribution defaults; dhcpcd always uses UID/GID 101. Networking
reads `/etc/clankerd/dhcpcd.conf` directly instead of generating configuration
under `/run`. Machine addresses, leases, resolver data, and hostname remain
runtime state.

The Linux test tools use one checked-in launcher with tool-name symlinks,
and a management-root link selected by Make. The DinD Dockerfile/entrypoint
live under `images/dind/`; the smoke Compose project is a checked-in fixture.

`build-support/artifact.mk` declares installed programs, dylibs, guest files,
and license outputs. Its recipes invoke `install`, `cp`, and `rcodesign`
directly; the VMM target declares its entitlements. The main graph supplies
completed inputs and publishes the staging tree only after installation,
signing, and the host import audit succeed.

`--guest-root DIR` / `CLANKERD_GUEST_ROOT` selects that complete filesystem.
`--network-executable PATH` / `CLANKERD_NETWORK` selects the native transport.
The build and runtime accept their current contracts only: no flat tool layout,
old CLI names, scalar-volume decoder, peer-opt-out adapter, implicit automatic
shared prefix, installed helper/controller, Make aliases or artifact-directory
migration is retained. Existing unrelated output directories are rejected.
Shared networks always persist their ULA /64; native readiness reports the
transport PID and must acknowledge that prefix before workload startup.

The earlier native PASS is baseline evidence. This cleanup changes runtime
interfaces and requires a fresh consolidated Mac run; see the current
[contract review](self-contained-build-review.md).
