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
scripts/m2-boot-check.sh
```

Expected: `vmctl exit code: 2`, console output containing the guestd error
`unknown argument --nope`, final `PASS`. By hand:

```sh
export CLANKERD_VMSPAWN=$PWD/build/rust/darwin-arm64/clankerd-vmspawn
export CLANKERD_GUESTD=$PWD/build/rust/linux-arm64/clankerd-guestd
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
