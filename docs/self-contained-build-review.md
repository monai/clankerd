# Self-contained build contract review

Review baseline: `ad21387`; finding locations refer to that revision.

Scope: the artifact graph, guest filesystem and population/boot interfaces,
native attachment/authorization/PF/recovery interfaces, their callers, tests,
acceptance scripts and active documentation. This caretaker review is in act
mode and follows the user's requirement that no old build or networking layout
or interface be preserved for compatibility.

## Contracts and tests

| Unit | Current contract | Tests / observations |
|---|---|---|
| `Makefile` | Exact locked inputs, hash verification before use; Cargo owns Rust freshness and packaging consumes its executable outputs | `test_build_graph.py`: verification, Cargo delegation, compiled/input changes, warm reuse, missing output |
| `extract-binary.py` | Keep files and guest links within the destination; retain APK scripts as metadata without execution | `test_binary_extraction.py`: guest links/modes, metadata, path/link traversal |
| `Makefile`, `build-support/artifact.mk` | Make owns packaging freshness through file prerequisites and plain stamps; failed signing does not replace its target file | `test_build_graph.py`: signing/hash/compiler failures, interrupted compilation, warm reuse and ordinary output directories |
| `assemble-management.py` | Preserve package paths/modes/links; overlay checked-in management identities, locked passwords and DHCP files; reject missing, non-executable or foreign programs | `test_management.py`: layout, fixed filesystem policy, invalid inputs; `test_build_graph.py`: static-input rebuild scope |
| `build-support/artifact.mk` | Explicit installed and signed outputs; one guest filesystem under `linux-arm64/management`; no flat executable copies or root aliases | `test_build_graph.py`: completed artifact layout, signing failure and warm reuse; signed cross-build |
| `guest_environment::place` | Copy and validate the same management filesystem for every VM execution context | `libclankerd/tests/vmspawn.rs`, `vmspawn_populator.rs`; native management/boot gates |
| `rootdisk`, `volume` | Execute packaged `sbin/mke2fs` and `usr/sbin/resize2fs`; preserve disk and volume behavior | guestd volume unit tests, `seam_b.rs`, libclankerd image/population tests; native boot/Docker gates |
| `SpawnSpec` | Ordered volume collection; reject unknown fields and non-collection encodings | `clankerd-proto/tests/spawn.rs` |
| `vmctl` runtime configuration | Exact artifact-relative defaults and explicit `--guest-root` / `--network-executable`; no layout search or tool-parent inference | CLI run/lifecycle/reimage/timestamp tests; native acceptance |
| virtual networks | Membership determines addressing; every shared resource has a persisted aligned ULA /64; host-only resources have no global prefix | `libclankerd/tests/networks.rs`: membership, prefix validity, persistence, allocation and errors |
| native readiness | A complete flushed record contains transport PID, valid interface data and the selected prefix | `net::vmnet` readiness table; `libclankerd/tests/net.rs` startup/failure tests |
| native authorization | Runtime-owned authorization; no installed executable or sudoers rule; transport and VMM remain unprivileged | native private-authorization, management ownership and revoked-credential recovery checks |
| PF | Caller/executable/socket ownership; mark each native bridge and deny cross-bridge egress; same-network membership remains usable | `clankerd-network::pf` ownership/rule tests; native-prefix routed packet/counter/lifecycle gate |
| recovery | Exact selected configuration and prefix survive attachment replacement; no automatic-prefix substitution | `libclankerd/tests/net_recovery.rs`; native recovery/lifetimes/prefix gates |
| Developer checks and coverage | Direct Cargo test/lint commands independent of artifact production; selected signed bundle for opt-in native checks; consistent coverage instrumentation | `test_developer_checks.py`, `test_rust_coverage.py`, Linux suite, native unit/bind entry points |
| acceptance process evidence | Recorded PID must belong to the requesting nonroot user and map bundled executable/libraries | `test_accept_vm_process.py`: owner/mapping classes; native management gate |
| gate runner | Retain logs/status, bounded cleanup, same authorizing terminal session, independent signal group | `test_acceptance_gates.py`: subprocess/session/timeout contracts |
| gate cleanup | Workload exit preserves management; removal stops only the recorded network process | boot-gate fixture; real process lifecycle checks; native boot gate |
| acceptance reporting | Numbered progress and long-step estimates; bounded failure context before verbose cleanup dumps | gate/smoke/summary tests; compact native entry point |

## Findings

| Test | Rule | Problem |
|---|---|---|
| `test_build_graph.py:171:test_legacy_bundle_is_retained_when_the_complete_artifact_is_published` | patchwork | Locked in old-directory migration instead of the publication ownership contract. |
| `clankerd-proto/tests/spawn.rs:15:volume_disk_decoding_accepts_absence_collections_and_legacy_specs` | patchwork | Promised a second wire encoding for an obsolete scalar field. |
| `libclankerd/tests/networks.rs:131:legacy_peer_opt_out_allocates_a_singleton_network_instead_of_reusing_the_subnet` and `networks.rs:185:repeated_legacy_singleton_updates_preserve_membership` | patchwork | Preserved a per-machine adapter instead of explicit resource membership. |
| `test_acceptance_gates.py:111:NetworkGate` | bug-shaped | Extracted production shell source and faked broad pgrep success around one report. |
| `test_accept_vm_process.py:14` call/call-count assertions | tautological | Asserted the diagnostic implementation's internal call sequence. |
| `test_rust_test.py:52:test_native_vm_checks_build_the_selected_bundle_first` | tautological | Asserted mocked runner calls rather than executing the tool boundary. |
| `test_build_graph.py:134:test_build_works_without_the_loader_api_removed_in_python_315` | bug-shaped | Patched an obsolete interpreter method instead of asserting artifact behavior on supported interpreters. |
| `test_network_pf.py:1` | patchwork | Tested the retired installed Python controller rather than the native controller. |
| — | missing test | Package layout/provisioning had no direct contract suite, allowing root aliases to appear valid. |
| — | missing test | Implicit shared resources retained a separate automatic-prefix path with duplicated recovery-prefix state. |

## Changes

| Removed | Added | Code change | Contract aspect |
|---|---|---|---|
| Root tool aliases and flat guestd copies | Packaged-layout and artifact-layout assertions | Tools and init use `sbin/` and `usr/sbin/`; config uses `etc/clankerd/`. | One management filesystem. |
| Separate flat local-population layout and executable-parent inference | Same-layout local fixtures | VM/population constructors consume a filesystem root; CLI explicitly selects it. | Execution context changes transport, not layout. |
| Legacy volume decoder | Collection and invalid-schema cases | `SpawnSpec` accepts one current schema. | Unambiguous internal protocol. |
| Peer opt-out adapter and automatic-prefix branch | Resource-mode/prefix tests | Shared resources always own their prefix; recovery uses that configuration. | Addressing follows membership. |
| Process-list discovery of transport PID | Readiness identity/interface table | Native transport publishes its PID; the host records it directly. | Explicit native startup protocol. |
| Python PF controller, its suite, installed-helper diagnostics/installers and superseded PF policy experiments | Native PF ownership/policy tests | Keep one bundled native controller. | Runtime-owned host policy. |
| Make aliases and directory migration | Ownership rejection and existing atomic-publication tests | One build interface; no old-artifact conversion. | Complete output publication. |
| Source-snippet cleanup test and mock-call assertions | Real process lifecycle and tool-boundary tests; owner/mapping input classes | Tests assert lifecycle and evidence, not implementation choreography. | Observable integration outcomes. |
| Homebrew loader-path restoration and dependency repair | Bundled artifact checks | No runtime or test wrapper reinstates installed-library search paths; build-only linker names stay outside the runtime library directory. | Bundle dependency closure. |

## Validation

Validation runs use isolated Linux Cargo/build/coverage directories under `/tmp`;
the workspace also contains Mac objects, which are not reused for Linux builds.
The native acceptance script performs the current Python contract suite before
building and then runs the relocated artifact through all native gates. It
prints compact numbered output and retains verbose evidence.

Controlled mutations of management assembly (root aliases, account collision,
foreign architecture, dropped permissions) and native PF (foreign owner, wrong
socket, inverted cross-network policy) are rejected by their contract tests.
LLVM line/branch coverage and Python line/branch coverage were run; privileged
Apple API/PF paths require actual native observations rather than Linux coverage.
Historical caretaker checkpoint results (before removal of the custom cache): 361 Rust tests pass (7 native-only tests ignored on Linux), 33 Python
tests pass, strict clippy and formatting pass, the complete ARM64 cross-build
signs and passes the host import audit, and a warm build emits no work while
retaining the generation. Full Rust line/branch coverage is 73.62% / 61.98%;
Python statement/branch coverage is 84.18% / 72.35%, including subprocess
assembly/extraction, with separate retained JSON data. The Mac runner executes
every current Python contract test.

A final packaging pass keeps liblzma's build-only linker name under pkg-config
metadata, outside the runtime library directory. Its version comes from the
locked package metadata. Make supplies that metadata directly to Cargo. Make and Cargo are the only
build freshness mechanisms; the earlier generation and inventory cache has
been removed.

The user-supplied full native PASS at `build/acceptance-20261018T164230Z` validates
the preceding implementation. It does not validate this interface/layout cleanup.
Run the current consolidated entry point on the Mac:

```bash
mise exec -- bash scripts/accept-self-contained-build.sh
```

## Open questions

| Unit | Aspect | What is unclear | Tests kept |
|---|---|---|---|
| Native runtime | Verification of this revision | The changed filesystem, readiness record and prefix contract need a fresh Mac run; no semantic fallback is retained while waiting. | All consolidated native management, bind, boot, command, prefix/PF, lifetime, recovery and Docker gates. |
| Networking | Additional platform observations | Sleep/wake, independent LAN peers and external IPv6 have not been established. | Dedicated bridged/host-only diagnostics and explicit coverage limits. |
