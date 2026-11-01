# Product surface and verification selection

The authoritative CLI request and metadata definitions are `crates/vmctl/src/contract.rs`. Public behavior is documented in `README.md`, `docs/machine-lifecycle.md`, `docs/virtual-networks.md` and `docs/docker-in-machine.md`. Library callers use `Engine` and `Machine` in `crates/libclankerd/src/engine.rs` and `machine.rs`. Protocol and direct Linux guest tests prove those boundaries, not the native hypervisor.

## Groups

These are the draft coverage boundaries. Execution coverage belongs to the lifecycle group and has no separate target. The Drive section in [execution and evidence](execution.md) records implemented assertions and planned groups.

| Group | Location and cost | Promises to observe |
| --- | --- | --- |
| Local checks | Linux, regular development | CLI grammar and results, protocol, library logic, local guest process behavior, Python build contracts, formatting and Clippy. |
| Coverage | Linux, extended | Instrumented workspace behavior. Root-only filesystem, native and opt-in registry paths remain separate. |
| Lifecycle | macOS, ordinary VM journey | Create saves stopped state. Start runs image command once per boot. Foreground run returns its exit status while VM remains running. Detached run returns. Exec still works after workload exit. Stop preserves disk and configuration. Start reruns command. Kill ends machine. Removal retains named volumes. |
| Lifecycle, execution coverage | macOS, ordinary VM journey | Image defaults and explicit entrypoint, argv, environment, user and workdir. Separate stdout/stderr, stdin, exit status, interactive PTY. Logs expose console output. Management works independently of workload filesystem. |
| Storage | macOS, ordinary VM journey | Bind reads/writes and read-only enforcement. Simplified and complete permission semantics. Named-volume data survives restart, machine replacement and reimage. Stopped update adds/removes mounts and sockets while retaining data and omitted configuration. |
| Networking | macOS, ordinary multi-VM journey | Resource creation/inspection/removal. Shared Internet, host-only host/peer access, same-network peers and cross-network isolation. Persisted single membership changes only while stopped. Assigned network removal is rejected. Host TCP/UDP and Unix socket access follow documented library bindings. |
| Recovery | macOS, longer processes | Machine restart policies, explicit-stop intent, network and forwarding recovery, workload survival where promised. Address renewal and expiry need bounded waiting and prerequisite descriptions. |
| Workloads | macOS, longer processes | Documented Docker/Compose workload inside a real clankerd VM, published services, persistent workload data and reimage. |
| Distribution | macOS, longer build | Signed complete bundle builds and runs after relocation. Installed runtime libraries remain required. |
| Bridged networking | macOS, opt-in | Explicit physical interface attachment and LAN connectivity under the operator's actual network conditions. |

For CLI/output changes select local checks plus the host group using the changed command. For guestd/protocol changes select local checks and lifecycle. For rootdisk/image changes select local checks and lifecycle/storage. For vmspawn or networking helper changes select lifecycle/networking and recovery when persistence or reconnection changes. For packaging changes select distribution. These selections may overlap.

## CLI affordances

| Resource | Actions |
| --- | --- |
| `machine` | `run`, `create`, `start`, `update`, `reimage`, `stop`, `kill`, `ls`, `inspect`, `logs`, `exec`, `rm` |
| `image` | `pull`, `ls` |
| `network` | `create`, `ls`, `inspect`, `rm` |
| `volume` | `create`, `ls`, `inspect`, `rm` |

Machine actions have root shortcuts. Machine `ls` uses shortcut `ps`; image `ls` uses `images`. Prefer canonical resource/action syntax in maintained journeys.

Global options precede the resource. Action options precede operands. After create/run IMAGE or exec MACHINE, remaining tokens are literal guest argv. Invoke a guest shell explicitly when shell interpretation is required. CLI settings override environment settings, then defaults.

Creation accepts `--name`, `--env`, `--user`, `--workdir`, `--entrypoint`, `--interactive`, `--tty`, `--stop-signal`, `--cpus`, `--memory`, `--restart`, `--pull`, `--network`, `--bridge-interface`, `--mount`, `--volume` and `--socket`. Exec has its own environment/user/workdir/stdin/TTY overrides. Update changes host settings plus `--mount-add`, `--mount-remove`, `--socket-add` and `--socket-remove`. Update and reimage require stopped machines. Network and physical bridge selections conflict. Inspect the actual command help before composing operands.

Image pulls target linux/arm64. Pull policies are missing, always and never. Restart policies are no, always, unless-stopped and on-failure with optional retry count. Restart policies apply to machine failure, not ordinary workload completion. Registry tests require real connectivity or an explicit registry fixture. Do not infer authentication support from historical scripts.

Bind mounts default to simplified permissions. Guest ownership follows the requesting user; chown does not persist ownership; chmod changes Mac permissions. Complete semantics store guest ownership and permissions in host extended attributes. Named volumes persist until explicit volume removal. Reimage replaces the machine root while retaining volumes and overrides.

## Structured observation

Run `"$vmctl" --describe-contract` by itself to obtain current command metadata, aliases, schemas, eligibility and output policies. Save it with evidence rather than copying schemas into this skill.

Lists support `--output json`. Inspects emit JSON arrays. Mutations support `--output json` and emit operation arrays. Image pull emits an image object or null on failure. Foreground run, exec and logs preserve original stream bytes; foreground run does not support JSON. Detached run supports JSON.

Capture stderr separately from stdout. Exit 0 means success, 1 means operation or partial failure, and 2 means invalid invocation. Foreground run and exec return the guest process status when available. Batch operations attempt independent targets in input order without rollback. Check each operation and the final exit status. Timestamps are UTC epoch milliseconds; absent values are null.

## Unclear promises

Host sleep/wake, external IPv6 availability, registry authentication and exact recovery timing need an explicit documented contract and environment before becoming unconditional assertions. The presence of a script for a past failure does not define that contract. Public library forwarding may exceed CLI affordances; use library-level tests for that surface instead of inventing CLI flags.
