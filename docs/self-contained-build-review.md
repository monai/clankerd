# Implementation review and validation

Review baseline: `63850d00d5b5c9eb89d3090fbece149de38964db`.
Two independent reviewers applied the code-review skill's Standards and Spec axes.

## Standards

No documented-standard or membership-ADR violations were identified.

| Finding | Resolution |
| --- | --- |
| Authorized requests re-executed a user-writable bundle pathname as root | The session freezes its authorized networking code into a root-owned private temporary directory, cleans it on ordinary failure/exit, and executes that snapshot. Assembly enforces OS-only imports for this executable. |
| Workload paths could follow image symlinks into management before switching roots | Paths now interpret absolute symlinks relative to the workload root. Mounts, resolver publication, account lookup and Unix sockets use the appropriate filesystem view. |
| Namespace cleanup used a literal path after resolving its image symlink | Cleanup retains the resolved path inside the new root. The native fixture includes this symlink case. |
| Possible duplicated process-ownership parsing | Nonblocking heuristic retained: the library recognizes launchers during failed startup, while the privileged PF controller validates actual caller-owned attachments. No shared public testing API was added. |

The snapshot freezes the file read during initial authorization; it is not a
claim of cryptographically identifying the already-running image during a
concurrent edit. Later bundle replacement cannot alter that authorized session.

## Spec

| Finding | Resolution |
| --- | --- |
| Password entry consumed the attachment's ten-second readiness deadline | Authorization completes synchronously before native startup timing begins. The Mac runner exercises first use without warming sudo credentials, and replacement after revoking those credentials. |
| Native execution cannot be established on the Linux host | Kept explicitly pending; the consolidated Mac runner builds, relocates and exercises the production artifact, without the experiment's management overlay or installed helper. |

No remaining concrete implementation defect was reported in the reviewed
changes. This does not substitute for native acceptance.

## Local validation

Execution environment: ARM64 Linux container; Rust 1.98.1, Zig 0.15.2,
cargo-zigbuild 0.23.4 and rcodesign 0.29.0 through the existing mise installation.
Cargo uses isolated `/tmp/clankerd-self-contained-*` targets because the mounted
workspace produced stale file handles during compilation. Existing user artifacts
and unrelated untracked image files were retained.

- Full `make test`: 356 Rust tests pass (7 native tests ignored), including the updated VMM/population/CLI
  fixtures; native-only tests remain ignored on Linux. Privilege-gated paths
  do not establish actual VM observations.
- Python suite: 31 tests pass. Make graph behavior, controller prior-art contracts and Rust
  runner tests pass. The graph suite covers legacy publication, source additions/
  removals, changed binary declarations, corrupt/bad hashes, missing outputs,
  failed signing, interruption and concurrent invocations.
- Workspace formatting and clippy with warnings denied pass.
- The complete Mac/Linux artifact cross-builds and signs successfully from the
  locked binary inputs. Every Mach-O import and guest executable architecture
  is audited during assembly.
- A warm `make -j8 build` retains the exact generation and inventory and emits
  no compilation, assembly or signing output.
- Python compilation and consolidated shell-script syntax checks pass.

## Native acceptance

Run on the macOS 15 Apple Silicon host:

```sh
mise exec -- bash scripts/accept-self-contained-build.sh
```

The runner retains `result.json`, management observations, loaded-library maps,
input/image identities and gate logs beneath `build/acceptance-<UTC timestamp>`.
Networking/PF, authorization, actual guest privilege and mount behavior, storage,
Docker/Compose and relocation are **pending native execution**. Sleep/wake and
external IPv6 remain unverified unless observed separately.

## Separate findings

Older other-VMM and exploratory PF diagnostic scripts still refer to installed
helper paths and historical host policy. They are not invoked by the integrated
acceptance runner; general migration of those experiments remains outside this
refactor. No unrelated image/Dockerfile changes were included.

## Python 3.15 compatibility follow-up

The first Mac run reached the graph tests and exposed Python 3.15's removal of
`SourceFileLoader.load_module`. The phase runner now uses `exec_module`.
A regression test removes the obsolete API on older interpreters and builds
through the real Make entry point. All eight graph tests pass on the available
Python 3.13 and 3.14 interpreters; the full Python suite passes 31 tests.
Concurrent tests collect both subprocess results before asserting, preventing
unreaped processes and open-pipe warnings when a build fails. An actual Python
3.15 binary was unavailable in the Linux download catalog; Mac execution remains
pending a rerun of the same consolidated acceptance command.

## Native linker follow-up

The macOS 15 run passed all eight graph tests under Python 3.15, then Apple's
TAPI parser rejected the plural `compatibility-versions` key in the libkrun
stub. It now uses the TAPI v4 singular `compatibility-version`. The native
acceptance entrypoint links a minimal caller against the stub before Cargo.

The same log showed Cargo selecting Homebrew's liblzma. Host Rust compilation
now depends on the relocated pinned libraries and supplies isolated liblzma
pkg-config metadata; vmctl also searches its bundled library directory. Phase
subprocesses clear Make jobserver variables because Python closes those file
descriptors. The graph fixture checks those compilation environment boundaries.
Actual Apple linker and VM acceptance still require the native script run.

Validation: all 31 Python tests, shell syntax, Rust formatting, and the signed
ARM64 cross-build/import audit passed. Both vmctl and clankerd-vmspawn import
`@rpath/liblzma.5.dylib` from the pinned bundle.

## Management process evidence follow-up

The next macOS 15 acceptance run passed native stub linking, all eight graph
checks, compilation, signing, import closure, vmctl execution, and the guest
management namespace/DHCP checks. The host process check then found no command
matching the long bundle path in `ps` output. That check depended on unbounded
command text without requesting wide output; the captured failure did not
include the process listing needed to confirm truncation.

Acceptance now selects the VMM PID from `vmctl inspect`, checks that exact PID's
numeric owner, and verifies its executable and bundled libraries with `vmmap`.
It preserves inspect, owner, and mapping evidence before assertions. Regression
checks cover a long bundle path, root ownership rejection, and Homebrew import
rejection, and run in the same Mac entrypoint. Tool version probes also clear
stale Make jobserver variables; the graph fixture reproduces that environment
failure before the fix. Full native acceptance still requires a Mac run.

## Native gate progress and boot cleanup follow-up

The macOS acceptance run at 20261018T104144Z passed all management checks,
including shell-free and hostile glibc workloads, mount separation, unprivileged
VMM ownership, bundled library mapping, attachment recovery after credential
revocation, stop/start, identity overrides, and image identity retention.
The command-system gate also passed. The boot gate received the correct exit
code 2 and console output, then failed because it tried to remove the machine
while management remained running. A script-level fixture reproduced that
exact failure; the gate now explicitly stops it before removal and includes it
in failure cleanup.

The prefix gate's progress was redirected entirely to its evidence log, so the
terminal alone cannot establish whether it hung. Acceptance now streams gate
output and emits 30-second heartbeats, with a configurable 30-minute deadline
and bounded process-group cleanup. Runner checks cover retained stdout/stderr,
exit status, timeout, and execution of the shell cleanup trap. The user subsequently confirmed that the native-prefix gate passed and the
lifetimes gate began. Corrected boot/image/volume/network and remaining gates
still require Mac verification. All 37 Python checks and shell syntax checks pass.

## Docker smoke CLI and progress follow-up

The user confirmed native-prefix, lease/RA lifetimes, and recovery gates passed.
The Docker gate stopped at `vmctl image inspect`, which is absent from the CLI;
it now records cached image metadata with `vmctl image ls --output json`.
The existing smoke-script fixture now rejects the unsupported command: its
clean run failed with the exact reported CLI error before the fix and all three
smoke-script tests pass afterward. Assertions also cover numbered progress and
long-running labels. Acceptance numbers its nine preparation steps and selected
gates, and includes estimated durations for slow operations.

The reported EOF did not reproduce in syntax checks. The entrypoint had been
edited while Bash was running its earlier version, which can change the unread
remainder of a shell script. It now executes its initial contents from memory;
a regression replaces the script file while paused and verifies that its
original continuation completes. Native corrected boot and Docker gates remain
pending; passed native gates need not be rerun to investigate those failures.

## Final boot gate sidecar check

The latest native run passed management and Docker smoke. The remaining boot
gate failure was its broad `pgrep clankerd-network.*<state-directory>` after
removing only the net machine. The gate still has other running machines
(probe/override), so their legitimate sidecars satisfy that pattern. A fixture
reproduced this failure and now verifies removal using net's recorded network
PID while an unrelated process remains alive. The check waits up to five
seconds and treats zombies as stopped, recording live process details on failure.

Acceptance now prints compact progress and automatically emits the retained
paste-summary at exit. Full output stays in acceptance.log and per-gate logs;
ACCEPTANCE_VERBOSE=1 restores verbose terminal output. The native corrected
boot gate remains pending.
