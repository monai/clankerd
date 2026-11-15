# Networking lifecycle cleanup verification

Implementation base: `83cf4cde1df7ce6dce50ea7619474ce6ab9249ec`.
The rejected implementation is preserved in `4463b72`; `a6ac83f` restores the base tree.
Implementation began in a clean detached worktree at the base.

## Design comparison

Creation moves from the client to `Attachment::open` in the machine process.
Create returns its packet descriptor and binds ownership to that connection.
Attach, the waiting packet socket, and optional connection ownership are deleted.
Boot settings carry configuration rather than an attachment generation. The machine
writes the existing generation lease before boot while retaining its connection;
validation, publication, or boot failure drops that owner.

Necessary additions are cleanup error propagation through Stop/remove, process
termination on required helper cleanup failure, and preserving saved PF references
until release succeeds. Newly acquired references are saved before applying rules,
so a failed rollback can use the same startup reset. There is no timer, retry
registry, new journal, recovery format, or automatic network recreation.

Policy restarts require confirmed attachment ownership at exit observation. Because
ordinary machine connection teardown can precede observation, unexpected native
machine exits can conservatively require manual restart even when the helper did
not fail. Requested shutdown does not receive an ownership-loss diagnostic. This
avoids rerunning workloads when helper recovery has already erased the attachment;
no extra persistent helper-failure marker is introduced.

An initial failure to save a newly acquired PF reference combined with failed release
cannot be repaired by the existing state file: it cannot record evidence when its
storage is unavailable. The helper terminates and reports the release error. No
claim is made that this pre-existing acquisition/storage failure is crash-safe;
solving it requires a broader acquisition recovery design outside this change.

## Local evidence

Linux workspace typechecking, the full Rust workspace tests, focused network lifecycle
and helper tests, formatting and Clippy are run for this change. The focused lifecycle
fixture models machine-side creation; it does not execute XPC or native vmnet.
Python build contracts: 53 tests, one macOS-only skip.

Cross-target typechecking passed for `clankerd-network` and
`clankerd-network-ipc`, including their macOS tests. Full-workspace cross-checking
was blocked by third-party C dependency compiler configuration. Cross-checking does
not establish macOS runtime behavior.

PF reset tests reproduce failed release, assert unchanged saved evidence, and then
complete recovery after removing the fault. A subprocess test checks helper failure
and its manual-restart diagnostic. Absent-reference handling is limited to valid
numeric tokens and the native DIOCSTOPREF EINVAL/ENOENT errors, with C locale output.
Native semantics are from [Apple's XNU DIOCSTOPREF implementation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/net/pf_ioctl.c).
Other errors remain failures. Native pfctl output matching still requires host proof.

## Native handoff

Use the final commits accompanying this file, with a clean tree (`git rev-parse HEAD`
and `git diff --binary` identify the source). Run on Apple Silicon macOS 15 and a
supported newer host, with native dependencies from `docs/native-build.md`.
Use an idle or isolated host: helper failure interrupts every helper client.

```sh
mise exec -- cmake --preset default
mise exec -- cmake --build --preset default --target install-network-helper
mise exec -- cmake --build --preset default --target verify-network-helper
mise exec -- cmake --build --preset default --target verify-host-lifecycle
```

`verify-network-helper` runs the existing native XPC lifecycle test. Its updated
assertions cover usable ARP packets from Create, owner disconnect with the packet
FD retained, generation replacement, stale Stop safety, and independent ownership.
The lifecycle target records revision, patch identity, binary identity and journey
output. Retain signed build output and OS version independently of probe state.

Still unverified: connection death during creation/response delivery; actual machine
boot failure after creation and process-exit resource cleanup; native helper failure,
failed PF release, startup refusal, and subsequent recovery with saved artifacts;
Shared IPv4/IPv6 Internet and host access, Host-only host/permitted-peer access,
cross-network isolation/source validation, and Bridged interface/LAN connectivity.
There are no executable stable networking/recovery/bridge journey groups beyond
the XPC test. Do not report the ordinary lifecycle target as covering them.
These need host execution with peer services and explicit LAN/Internet prerequisites.
Inspect probe-owned vmnet/bridge and PF artifacts before and after, including the
saved reference after failed release. Keep native failure injection confined to an
isolated test build or existing command boundary; there is no production fault switch.
Clean only probe-owned machines, networks, volumes and firewall artifacts. Return
command exit statuses, stdout/stderr, console/helper logs, and resource snapshots.
