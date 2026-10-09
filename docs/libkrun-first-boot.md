# First real boot on libkrun (M2 hand-off for ticket 03)

The prototype has booted real VMs on the developer's Apple Silicon Mac.
The boot, image, volume, networking and Docker smoke checks passed; the manual
error-message checks and the remaining unchecked items below are still open.

## Validation record (2026-10-09)

Results reported by the developer from the Mac:

| Check | Result |
| --- | --- |
| `scripts/m2-boot-check.sh boot` | Passed: guest exit code 2 returned to the host |
| `scripts/m2-boot-check.sh image` | Passed: image defaults, overrides, cgroups, clock, graceful stop and forced-stop timeout |
| `scripts/m2-boot-check.sh volume` | Passed: formatting, persistence, growth, removal and bind mounts |
| `scripts/m2-boot-check.sh net` | Passed: DHCP, DNS, HTTPS, forwarding and blocked backend endpoints |
| `scripts/m2-smoke-dind.sh` | Passed after the forced-removal race fix: Docker, Compose, port forwarding, host gateway, restart, image change and cached create |

The Docker smoke used `127.0.0.1:5050/clankerd-dind:smoke`, derived from
`docker:29-dind` with nftables and volume-backed Docker storage. It ran Docker
29.9.0; the pulled arm64 image digest was
`sha256:a16feeffaa4f99282659d49a8118f8765ec89822ff77e3b328ff50a6118e887b`.
This does not certify Compose availability in `ghcr.io/monai/clankers:slim`.

Linux verification passed 285 test results, strict lint, Darwin cross-linking,
signing and binary verification. Source coverage was 77.89% lines and 62.28%
branches; see [Rust testing](rust-testing.md). Mounted population still needs
loop devices and mount permissions, unavailable in the agent's container.

Shutdown acknowledgments replaced the fixed linger; measured shell shutdown
fell from 422 ms to 125 ms on the Mac before temporary timing code was removed.
Forced removal now waits for helper exit, serializes lifecycle cleanup and
keeps the reaper's diagnostic file open so a late write cannot recreate deleted
state. The Linux contract test covers 100 removals, including 50 through a
reattached engine; the final Mac Docker smoke also passed forced removal.

## 1. Install

```sh
# Homebrew 6 asks you to trust third-party taps.
brew tap slp/krun
brew trust slp/krun
brew install libkrun            # 1.19.x; pulls in libkrunfw (stock kernel)
ls /opt/homebrew/lib/libkrun.1.dylib /opt/homebrew/lib/libkrunfw.5.dylib

mise install                    # rust, zig, cargo-zigbuild, rcodesign (pinned)

# ELF inspection for the Linux guest binaries (readelf or greadelf).
brew install binutils
export PATH="$(brew --prefix binutils)/bin:$PATH"
```

## 2. Build, sign, verify

```sh
mise exec -- make rust
```

`make rust` cross-links with zig, ad-hoc signs `clankerd-vmspawn` with
`crates/clankerd-vmspawn/entitlements.plist` (`com.apple.security.hypervisor`,
`com.apple.security.cs.disable-library-validation`) and runs
`scripts/rust-verify.sh`. Outputs: `build/rust/darwin-arm64/{vmctl,clankerd-vmspawn}`
and `build/rust/linux-arm64/clankerd-guestd`.

The Linux musl guest links with Rust's bundled `rust-lld` and musl libraries;
Zig is used for the Darwin binaries and the guest filesystem tools.

On macOS the build selects the installed Apple SDK with
`xcrun --sdk macosx --show-sdk-path`; Xcode Command Line Tools must be installed
(`xcode-select --install` if needed). The minimal SDK stubs under
`scripts/macos-sdk-stubs` are used only for cross-linking on Linux. They cannot
serve as the SDK for native macOS Rust build scripts: doing so causes
`ld: library 'System' not found`.

libkrun is linked against `crates/libkrun-sys/stubs/libkrun.tbd`, a text stub
with install name `/opt/homebrew/lib/libkrun.1.dylib`. This is what lets zig
link without a macOS SDK (the earlier xcrun/SDK problem); dyld loads the real
Homebrew dylib at run time. If you add a libkrun function to
`crates/libkrun-sys/src/ffi.rs`, add its `_symbol` to the stub too.

## 3. Run

```sh
scripts/m2-boot-check.sh boot      # ticket 03 only; no network, no image
```

Expected: `vmctl exit code: 2`, console output containing the guestd error
`unknown argument --nope`, final `PASS`. (`scripts/m2-boot-check.sh` with no
argument also runs the ticket 05 and 09 checks, see sections 7 and 8.) By hand:

```sh
export CLANKERD_VMSPAWN=$PWD/build/rust/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$PWD/build/rust/linux-arm64/clankerd-guestd
# Since ticket 05 vmctl boots images by default; this switch keeps the
# ticket 03 behaviour (the boot directory is the root).
export CLANKERD_BOOT_DIR_ROOT=true
build/rust/darwin-arm64/vmctl run --name t1 boot-dir /clankerd-guestd --nope; echo $?   # 2
build/rust/darwin-arm64/vmctl logs t1
```

The guest root is the machine's `boot/` directory (guestd and `workload.json`
only), so for now the command must be a file in it; images arrive with ticket
04. On macOS `vmctl` picks the libkrun backend by itself and finds the helper
and guestd next to it (`../linux-arm64/`); override with `CLANKERD_VMSPAWN` and
`CLANKERD_GUESTD`.

State lives in `~/Library/Application Support/clankerd/machines/<id>/`:
`console.log` (what `vmctl logs` prints), `vmspawn.log` (helper stderr, read
when the helper dies), `vmspawn.json`, `boot/`.

Failed boot checks preserve their temporary state directory and print its path,
along with the helper, console and gvproxy logs. `vmspawn-exit` records the
helper's exit code or terminating signal. Failed population boots separately
preserve their temporary boot directory and include its path in the error.
For a silent Mac crash, inspect the newest `clankerd-vmspawn` report under
`~/Library/Logs/DiagnosticReports/` for its exception, termination reason and
crashed thread.

libkrun loads `libkrunfw.5.dylib` by filename at runtime. On macOS the library
sets `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib:/usr/local/lib:/usr/lib` when
launching vmspawn, unless the caller provided its own value. If you override
this variable, include `/opt/homebrew/lib`. A missing search path can produce
`Couldn't find or load libkrunfw.5.dylib` even when Homebrew installed it.

## 4. Check the error messages by hand

| Do this | Expect from `vmctl run ...` |
|---|---|
| `brew unlink libkrun`, then run | `libkrun is not installed; run brew tap slp/krun && brew install libkrun` (fast, not a 30 s timeout) |
| `codesign --remove-signature build/rust/darwin-arm64/clankerd-vmspawn`, then run (a bare unsigned binary may be killed by macOS instead; if so, re-sign without `--entitlements` using `codesign --force -s -`) | `the hypervisor entitlement is missing: ... make rust-sign` |
| `brew unlink libkrunfw`, then run | `libkrunfw ... brew install libkrunfw` |

Restore with `brew link ...` and `make rust-sign`.

## 5. What is unverified (look here first if the boot fails)

Initial hand-off assumptions from `memory/research-libkrun.md` and the libkrun
1.19 header. See the validation record for tested paths; the unchecked manual
checks remain open:

1. **guestd as PID 1.** `krun_set_root` makes libkrun inject `/init.krun`, which
   should exec `/clankerd-guestd` because of `krun_set_exec` plus
   `KRUN_INIT_PID1=1` (passed in the env array). If guestd runs as a child of
   init.krun instead, the boot still works; check `ps` inside via logs. If
   `KRUN_INIT_PID1` is ignored, look at `init.c` for the right switch.
2. **`krun_set_exec` argv.** Passed without argv[0]
   (`--config /workload.json --vsock-port 1024`). If guestd reports an
   unknown argument that is the program path, prepend it.
3. **The boot directory is not read-only.** `krun_set_root` has no read-only
   switch; guestd never writes there, but the mount is writable. A
   `krun_add_virtiofs3(..., read_only)` root is the follow-up.
4. **Exit code path.** The exit code comes from guestd's `Exited` event,
   recorded by the helper in `exit`; libkrun's own exit status is ignored.
   guestd powers off with `reboot(RB_POWER_OFF)` when PID 1, after subscribers
   have read the event.
5. **vsock proxy.** libkrun creates a unix socket (`<runtime>/<id12>.vsock`,
   `listen=true`) for guest port 1024; the helper proxies the `LISTEN_FDS`
   socket (`<runtime>/<id12>.sock`) to it. A connection made before guestd
   listens is closed, and the library retries.
6. **Hypervisor probe.** `hv_vm_create`/`hv_vm_destroy` run in the helper
   before libkrun, via dlopen of Hypervisor.framework, to turn `HV_DENIED` into
   the entitlement message. libkrun's own failure would otherwise be a crash.
7. **Console.** `krun_set_console_output` is expected to capture kernel and
   init output in `console.log`. If the log is empty, try
   `krun_set_kernel_console` / the virtio console variants.
8. Unix socket paths must stay under 104 bytes; the runtime directory is
   `$TMPDIR/clankerd-<uid>` on macOS, which fits.
   Before a new workload boot, the library removes the previous libkrun vsock
   listener path. Libkrun can leave this socket file behind after power-off;
   leaving it in place makes stop/start fail with `krun_add_vsock_port2:
   File exists`. The previous helper has exited before a restart is allowed.

## 6. Ticket checklist for you

- [x] `vmctl run` of a command in the boot directory returns its exit code from a real VM
- [x] `vmctl logs NAME` shows kernel and guestd console output
- [ ] The three error messages in section 4 appear as described
- [ ] Items in section 5 confirmed or corrected

## Shutdown acknowledgments

The fixed 300 ms linger has been replaced by acknowledgments. The helper
atomically records the workload's exit code before acknowledging it; the
local-process backend writes its exit file directly. The library confirms
that startup and forwarding initialization are complete. Foreground runs
reserve their attachment with `Machine::start_attached`, and the stream client
acknowledges terminal status after receiving it. Guestd can then drain existing
subscribers and shut down immediately. Missing clients retain a bounded
five-second drain deadline, and the normal process grace periods and filesystem
cleanup still run. Rebuild host and guest binaries together for this protocol.

## 7. Boot a machine from an OCI image (M2 hand-off for ticket 05)

Built and tested on Linux only (seam A with the local-process stand-in, plus
unit tests of guestd's boot pieces). Nothing below has run in a VM.

What changed: `vmctl create/run` pull the image, build a cached base root disk
(one *population boot*: `clankerd-vmspawn --populate` boots guestd against a
blank disk, the host streams the merged tar over vsock, guestd runs the static
`mke2fs` and unpacks), clone it per machine (APFS `clonefile`) and boot it.
guestd (`--boot`, PID 1) mounts `/dev/vda`, pivots into it, mounts
/proc /sys /dev /dev/pts /dev/shm and cgroup2, enables the cgroup v2
controllers for children of the root cgroup, sets the clock from the host,
writes `/.clankerdenv`, then runs ENTRYPOINT + CMD (image config merged with
`--entrypoint`, trailing args, `-e`, `-u`, `-w`). The boot directory stays at
`/run/clankerd/boot` (read-only). `vmctl stop` asks guestd to SIGTERM the
workload; when it exits guestd SIGTERMs/SIGKILLs everything, syncs, unmounts,
remounts `/` read-only and powers off. After `-t` seconds (default 10) the
helper is SIGKILLed instead (exit code 137).

### Run it

```sh
make rust
scripts/m2-boot-check.sh           # ticket 03 check, then the ticket 05 checks below
```

The script pulls `ghcr.io/monai/clankers:slim` (override with `IMAGE=`), so the
first run builds the base disk: expect a minute or two for the population boot.
The base disk is cached in `~/Library/Caches/clankerd/bases/`; later creates
are clones. By hand, the same checks:

```sh
export CLANKERD_VMSPAWN=$PWD/build/rust/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$PWD/build/rust/linux-arm64/clankerd-guestd   # mke2fs etc. must sit next to it
unset CLANKERD_BOOT_DIR_ROOT
vmctl=build/rust/darwin-arm64/vmctl
img=ghcr.io/monai/clankers:slim

$vmctl run -d --name dev $img sleep infinity            # prints the machine id
$vmctl exec dev sh -c 'id -un; pwd; env | sort; ls -la /.clankerdenv'
$vmctl exec dev cat /sys/fs/cgroup/cgroup.subtree_control   # cpuset cpu io memory pids ...
$vmctl exec dev date; date                              # clocks agree
$vmctl stop dev; $vmctl ps -a; $vmctl logs dev | tail   # Exited (143), shutdown lines in the log
$vmctl run $img true; echo $?                           # 0, machine reaches exited
$vmctl run -e FOO=bar -u root -w /tmp --entrypoint sh $img -c 'id -un; pwd; echo $FOO'
$vmctl run -d --name stubborn --entrypoint sh $img -c 'trap "" TERM; sleep infinity'
time $vmctl stop -t 2 stubborn                          # ~2 s, then Exited (137)
$vmctl kill dev 2>&1 | head -1                          # error: not running (use run -d again first)
$vmctl rm -f dev stubborn
```

The console log (`vmctl logs NAME`) is where a failing boot shows up: guestd
prints `clankerd-guestd: switching to the root disk: ...` style errors and the
shutdown steps (`stopping processes`, `syncing and unmounting`, `powering off`).

### Unverified (look here first if the boot fails)

1. **`krun_add_disk2` signature.** Declared as
   `(ctx, block_id, path, disk_format u32, read_only bool)` with format 0 = raw,
   from the 1.19 header notes in `memory/research-libkrun.md`. The disk is
   expected to show up as `/dev/vda` (first virtio-blk). If libkrun rejects the
   call, compare with `libkrun.h` and fix `crates/libkrun-sys/src/ffi.rs`
   (and the symbol in `stubs/libkrun.tbd`).
2. **`/dev` in libkrun's init.** guestd waits up to 10 s for `/dev/vda`; if it
   is missing it tries to mount devtmpfs on `/dev` itself. If both fail the
   error says the root disk did not appear.
3. **Pivot sequence.** `mount /dev/vda /newroot`, tmpfs on `/newroot/run`, a
   bind of the old root at `/newroot/run/clankerd/boot`, `MS_PRIVATE` on `/`,
   `pivot_root`, then the old root is detached. `pivot_root` needs the old root
   not to be an initramfs; libkrun's root is virtio-fs, so it should work. If it
   fails with EINVAL, the error is printed by guestd (`pivot_root: ...`).
   Alternative: `krun_set_root_disk_remount`.
4. **ext4 and cgroup2 in the stock kernel.** Both are built in according to the
   libkrunfw config notes. The `hugetlb` controller is absent, so the delegated
   list is `cpuset cpu io memory pids` (not all necessarily present).
5. **Clock.** guestd calls `clock_settime(CLOCK_REALTIME)` with the host time
   recorded when the machine started (libkrun may also run its own time sync).
   Failures are logged and ignored. Waking from Mac sleep is not handled yet.
6. **Population boot power-off.** After the host receives the `PopulateDisk`
   reply, it sends `FinishPopulation` on a new connection. Only then does
   guestd (PID 1) sync, unmount and power off, and the helper must then exit; the host
   waits 30 s for that and fails otherwise. If it hangs, the base disk is
   fine (the rename happens after), but the error will say the boot did not
   power off.

   Host and guest binaries must be rebuilt together for this handshake. The
   acknowledgment prevents VM power-off from discarding a queued vsock reply.
   The tar transfer uses stdin data and close frames from the stream codec,
   keeping the socket open for the reply. libkrun 1.19.6 treats a host socket
   EOF as a closed connection and resets it, so a write half-close produces
   `guestd closed the connection without replying` on the host and `Broken pipe`
   when guestd writes its reply. See [the upstream Unix proxy](https://github.com/libkrun/libkrun/blob/v1.19.6/src/devices/src/virtio/vsock/unix.rs#L196).
   Reply-write failures are also logged to the guest console.

   Libkrun's imago raw-storage backend may truncate a discarded zero range at
   the end of the backing file while retaining its in-memory disk capacity.
   Population restores the intended file length after the VM exits; workload
   starts restore the root disk's recorded capacity. Volume metadata also
   retains its logical capacity and restores the backing length at start.
   Cached ext4 bases shorter than their filesystem geometry are rebuilt
   automatically. This prevents the next VM seeing `bad geometry: block count
   ... exceeds size of device` after reopening the truncated backing file.
7. **Graceful stop.** guestd's `Shutdown` only signals the workload (SIGTERM);
   everything after that is the normal exit path. A workload that ignores
   SIGTERM is killed through the helper after the timeout (no in-guest SIGKILL
   escalation), as designed. Check `vmctl logs` for the teardown lines.
   The host waits for the VM helper to exit before reporting the machine
   exited, returning from `wait`/`stop`, stopping gvproxy or scheduling a
   restart. The workload's `Exited` event arrives before guestd's cleanup;
   ending the sidecar at that event can interrupt teardown. The stop timeout
   covers both workload termination and guest cleanup.
8. **Orphan reaping.** guestd as PID 1 reaps adopted zombies every 100 ms with
   `waitid(WNOWAIT)` and leaves the workload and exec children to their owners.
   `ps` inside should show no `<defunct>` entries after a `docker run`.

### Ticket 05 checklist for you

- [x] `vmctl run -d --name dev IMAGE sleep infinity` boots from the image; the workload runs as the image's USER in its WORKDIR with the merged env (exec `id`, `pwd`, `env`)
- [x] `vmctl run IMAGE true` exits 0 and the machine is `exited` in `ps -a`
- [x] `vmctl stop dev` is graceful for a plain `sleep` (shutdown lines and `Exited (143)`); `stop -t 2` kills a TERM-ignoring workload after 2 s (`Exited (137)`)
- [ ] A trapped TERM handler runs on the real VM before shutdown
- [x] `/.clankerdenv` exists in the machine
- [x] `cgroup.subtree_control` of the root cgroup lists the delegated controllers
- [x] A second `create` from the cached image is near-instant (no population boot)
- [ ] Items above confirmed or corrected

## 8. Named volume and bind mounts (M2 hand-off for ticket 09)

Built and tested on Linux only (seam A with the local-process stand-in, plus
unit tests of guestd's format/grow logic on regular files; the mount tests
need root and run in CI's root step). Nothing below has run in a VM.

What changed: `vmctl run/create -v data:/storage[:size=20G]` (or
`--volume-size 20G`) gives a machine one named volume. The library creates
`<state>/volumes/data/data.ext4` as a *sparse* file (default 16G); no ext4 work
happens on the Mac. `clankerd-vmspawn` attaches it as the second virtio-blk
device (`/dev/vdb`, after the root disk). guestd (`--boot`) formats it with the
static `mke2fs` when its first 64 KiB are blank (anything else that is not
ext4 is refused, never overwritten), mounts it at the target and, when the
requested size exceeds the filesystem, grows it online with the static
`resize2fs`. The host extends the sparse file at start when the size
increased; volumes never shrink. `vmctl rm` keeps the volume; `vmctl rm -v`
deletes it. `vmctl volume ls|create|inspect|rm` is the minimal management
surface. A volume can be mounted by one running machine at a time.
`-v ./dir:/path[:ro]` shares a host directory over virtio-fs (tag `bindN`),
mounted by guestd with `mount -t virtiofs`. `mke2fs` and `resize2fs` are
hard-linked from next to guestd into the machine's boot directory when a
volume is configured.

New volumes are root-owned. The persistence marker is written with
`vmctl exec -u root`; it remains readable by the image's user. Applications
running as `agent` can use directories explicitly created and assigned to
that user inside the volume. The bind-mount write probe also runs as root;
it verifies the mount's write path, while matching the host and workload's
numeric UIDs remains a separate ownership check.

### Run it

```sh
make rust
scripts/m2-boot-check.sh volume     # only the ticket 09 checks (pulls the image)
```

By hand:

```sh
export CLANKERD_VMSPAWN=$PWD/build/rust/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$PWD/build/rust/linux-arm64/clankerd-guestd
vmctl=build/rust/darwin-arm64/vmctl; img=ghcr.io/monai/clankers:slim
vol="$HOME/Library/Application Support/clankerd/volumes/data/data.ext4"

$vmctl run -d --name v1 -v data:/storage --volume-size 1G $img sleep infinity
ls -ls "$vol"                                   # 1 GiB apparent, little allocated
$vmctl exec -u root v1 sh -c 'grep storage /proc/mounts; df -h /storage; echo hi > /storage/f; sync'
$vmctl logs v1 | grep 'formatted new volume'
$vmctl stop v1; $vmctl start v1; $vmctl exec v1 cat /storage/f          # hi
$vmctl stop v1; $vmctl rm v1; $vmctl volume ls                           # data still there
$vmctl run -d --name v2 -v data:/storage:size=2G $img sleep infinity     # grows at start
$vmctl exec v2 sh -c 'df -h /storage; cat /storage/f'                    # ~2G, hi
$vmctl logs v2 | grep 'grew volume'
$vmctl rm -f -v v2; $vmctl volume ls                                     # gone

mkdir -p /tmp/shared; echo a > /tmp/shared/a
$vmctl run -d --name b -v /tmp/shared:/shared $img sleep infinity
$vmctl exec -u root b sh -c 'cat /shared/a; echo b > /shared/b'; cat /tmp/shared/b
$vmctl rm -f b
```

### Unverified (look here first if the boot fails)

1. **`krun_add_virtiofs` signature.** Declared as `(ctx, tag, path)` in
   `crates/libkrun-sys/src/ffi.rs`, from the header notes in
   `memory/research-libkrun.md`, and `_krun_add_virtiofs` added to
   `stubs/libkrun.tbd`. If linking or the call fails, compare with
   `libkrun.h` (there are also `krun_add_virtiofs2/3/4` with shm size and
   read-only flags; read-only is enforced by guestd's `MS_RDONLY` instead).
2. **Second disk is `/dev/vdb`.** Assumes virtio-blk devices are numbered in
   `krun_add_disk2` call order (root first, volume second). guestd waits up to
   10 s for the device; the error says the volume did not appear.
3. **Online `resize2fs`.** guestd mounts first, then runs
   `resize2fs /dev/vdb` (online resize needs no fsck). It needs online resize
   support in the stock kernel and a device at least as large as the
   filesystem (the host extends the file before boot).
4. **virtio-fs mount and ownership.** `mount -t virtiofs bind0 /target` needs
   virtiofs in the stock kernel (already used for the boot directory). File
   ownership seen in the guest comes from the Mac user (probably 501): check
   `ls -ln /shared` and whether the workload user can write.
5. **Several virtio-fs devices.** libkrun already serves the root as one
   virtio-fs device; extra tags should coexist but are untested.
6. **Sparse files on APFS.** `set_len` leaves holes; `du` should show little
   allocated after formatting (`lazy_itable_init`).
   While the VM is running, imago can truncate a discarded zero tail of the
   backing file without changing the live block-device capacity. The check
   therefore asserts the exact logical size from `vmctl volume inspect` and
   the actual guest device size from `/sys/class/block/vdb/size`, then checks
   filesystem growth and data persistence. Host EOF and allocation are reported
   separately; backing length is restored before the next VM opens it.
7. **Clean unmount.** The shutdown path already unmounts every non-pseudo
   mount (including the volume) before powering off; confirm there is no fsck
   on the next start after `vmctl stop`.

### Ticket 09 checklist for you

- [x] A new volume is created sparse, formatted in the guest, and mounted at the configured path (`scripts/m2-boot-check.sh volume`)
- [x] Data written to the volume survives stop/start and `vmctl rm` (no `-v`) followed by a new machine using the same volume
- [x] Increasing the size (`size=2G`) takes effect at the next start (`df` and the log line `grew volume`)
- [x] `vmctl rm -v` deletes the volume
- [x] A host directory bind mount is readable and writable from both sides
- [ ] Items above confirmed or corrected


## Forwarding lifetime

Published ports, host-gateway ports and socket bindings are owned by
`clankerd-vmspawn`. They survive `vmctl run -d`, later CLI commands and
reattachment, and are created again on a fresh start. The library controls
runtime bindings through a separate Unix socket (mode 0600) that is not mapped
into the guest. Dropping a runtime handle removes its helper-owned binding.
Rebuild both `vmctl` and `clankerd-vmspawn` for this protocol change.

## 9. Guest networking via gvproxy (M2 hand-off for ticket 08)

Every image-backed machine gets a virtio-net NIC served by its own gvproxy sidecar (pinned release, sha256-verified, cached under the cache dir; `--gvproxy PATH` / `CLANKERD_GVPROXY` overrides it). The sidecar starts before the VMM and is stopped with the machine.

### Run it

```sh
scripts/m2-boot-check.sh net
```

### Unverified (look here first if it fails)

1. **Pinned asset.** `GVPROXY_URL`/`GVPROXY_SHA256` in `crates/libclankerd/src/net/fetch.rs` must be the darwin arm64 binary; a checksum mismatch aborts `create`/`start`.
2. **NIC attach.** The NIC attach passed the Mac networking and Docker smoke checks; if eth0 never appears check `vmctl logs` and gvproxy's log in the machine directory.
3. **DHCP.** guestd speaks DHCP itself (static lease 192.168.127.2 keyed to the fixed MAC); check `ip addr show eth0` and `/etc/resolv.conf`.
4. **Control API block.** guestd installs an nft ruleset dropping 192.168.127.1:80 and 192.168.127.254 in `output` and `forward`; the image or kernel must provide nftables support.
5. **Sidecar lifetime.** `pgrep gvproxy` should be empty after `vmctl rm -f`.

### Ticket 08 checklist for you

- [ ] gvproxy is downloaded once and verified; a second run uses the cache
- [x] eth0 gets 192.168.127.2 by DHCP; public names resolve through the guest DNS configuration
- [ ] Host-specific or VPN DNS names resolve the same way inside the guest
- [x] The guest fetches an HTTPS URL
- [x] The guest cannot reach 192.168.127.1:80 or 192.168.127.254
- [x] `net.ipv4.ip_forward` is 1 before the workload starts
- [x] gvproxy starts and stops with each machine
