# First real boot on libkrun (M2 hand-off for ticket 03)

Everything below was built and tested on Linux only. Nothing here has booted a
VM yet; this document is how you do that on the M2.

## 1. Install

```sh
# Homebrew 6 asks you to trust third-party taps.
brew tap slp/krun
brew trust slp/krun
brew install libkrun            # 1.19.x; pulls in libkrunfw (stock kernel)
ls /opt/homebrew/lib/libkrun.1.dylib /opt/homebrew/lib/libkrunfw.5.dylib

mise install                    # rust, zig, cargo-zigbuild, rcodesign (pinned)
```

## 2. Build, sign, verify

```sh
make rust
```

`make rust` cross-links with zig, ad-hoc signs `clankerd-vmspawn` with
`crates/clankerd-vmspawn/entitlements.plist` (`com.apple.security.hypervisor`,
`com.apple.security.cs.disable-library-validation`) and runs
`scripts/rust-verify.sh`. Outputs: `build/rust/darwin-arm64/{vmctl,clankerd-vmspawn}`
and `build/rust/linux-arm64/clankerd-guestd`.

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
argument also runs the ticket 05 checks, see section 7.) By hand:

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

## 4. Check the error messages by hand

| Do this | Expect from `vmctl run ...` |
|---|---|
| `brew unlink libkrun`, then run | `libkrun is not installed; run brew tap slp/krun && brew install libkrun` (fast, not a 30 s timeout) |
| `codesign --remove-signature build/rust/darwin-arm64/clankerd-vmspawn`, then run (a bare unsigned binary may be killed by macOS instead; if so, re-sign without `--entitlements` using `codesign --force -s -`) | `the hypervisor entitlement is missing: ... make rust-sign` |
| `brew unlink libkrunfw`, then run | `libkrunfw ... brew install libkrunfw` |

Restore with `brew link ...` and `make rust-sign`.

## 5. What is unverified (look here first if the boot fails)

Written from `memory/research-libkrun.md` and the libkrun 1.19 header, not run:

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

## 6. Ticket checklist for you

- [ ] `vmctl run` of a command in the boot directory returns its exit code from a real VM
- [ ] `vmctl logs NAME` shows kernel and guestd console output
- [ ] The three error messages in section 4 appear as described
- [ ] Items in section 5 confirmed or corrected

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
6. **Population boot power-off.** After `PopulateDisk` replies, guestd (PID 1)
   syncs, unmounts and powers off, and the helper must then exit; the host
   waits 30 s for that and fails otherwise. If it hangs, the base disk is
   fine (the rename happens after), but the error will say the boot did not
   power off.
7. **Graceful stop.** guestd's `Shutdown` only signals the workload (SIGTERM);
   everything after that is the normal exit path. A workload that ignores
   SIGTERM is killed through the helper after the timeout (no in-guest SIGKILL
   escalation), as designed. Check `vmctl logs` for the teardown lines.
8. **Orphan reaping.** guestd as PID 1 reaps adopted zombies every 100 ms with
   `waitid(WNOWAIT)` and leaves the workload and exec children to their owners.
   `ps` inside should show no `<defunct>` entries after a `docker run`.

### Ticket 05 checklist for you

- [ ] `vmctl run -d --name dev IMAGE sleep infinity` boots from the image; the workload runs as the image's USER in its WORKDIR with the merged env (exec `id`, `pwd`, `env`)
- [ ] `vmctl run IMAGE true` exits 0 and the machine is `exited` in `ps -a`
- [ ] `vmctl stop dev` is graceful (console shows the shutdown lines, `Exited (143)` for a plain `sleep`, a trapped TERM handler runs) and `stop -t 2` on a TERM-ignoring workload kills it after 2 s (`Exited (137)`)
- [ ] `/.clankerdenv` exists in the machine
- [ ] `cgroup.subtree_control` of the root cgroup lists the delegated controllers
- [ ] A second `create` from the cached image is near-instant (no population boot)
- [ ] Items above confirmed or corrected

## 8. Guest networking via gvproxy (M2 hand-off for ticket 08)

Every image-backed machine gets a virtio-net NIC served by its own gvproxy sidecar (pinned release, sha256-verified, cached under the cache dir; `--gvproxy PATH` / `CLANKERD_GVPROXY` overrides it). The sidecar starts before the VMM and is stopped with the machine.

### Run it

```sh
scripts/m2-boot-check.sh net
```

### Unverified (look here first if it fails)

1. **Pinned asset.** `GVPROXY_URL`/`GVPROXY_SHA256` in `crates/libclankerd/src/net/fetch.rs` must be the darwin arm64 binary; a checksum mismatch aborts `create`/`start`.
2. **NIC attach.** The vfkit magic and virtio-net feature bits are untested against real libkrun; if eth0 never appears check `vmctl logs` and gvproxy's log in the machine directory.
3. **DHCP.** guestd speaks DHCP itself (static lease 192.168.127.2 keyed to the fixed MAC); check `ip addr show eth0` and `/etc/resolv.conf`.
4. **Control API block.** guestd installs an nft ruleset dropping 192.168.127.1:80 and 192.168.127.254 in `output` and `forward`; the image or kernel must provide nftables support.
5. **Sidecar lifetime.** `pgrep gvproxy` should be empty after `vmctl rm -f`.

### Ticket 08 checklist for you

- [ ] gvproxy is downloaded once and verified; a second run uses the cache
- [ ] eth0 gets 192.168.127.2 by DHCP; DNS resolves like on the host
- [ ] The guest fetches an HTTPS URL
- [ ] The guest cannot reach 192.168.127.1:80 or 192.168.127.254
- [ ] `net.ipv4.ip_forward` is 1 before the workload starts
- [ ] gvproxy starts and stops with each machine
