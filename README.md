# clankerd, hostctl and guestctl

Wires coding agents in one smolvm VM to apps, `.local` names and Chrome on the host.

- `clankerd`: host-only daemon, one per VM. Owns all state; answers mDNS, forwards TCP, runs Chrome, drives `smolvm`.
- `hostctl`: host-side client. Manages the VM and the daemon, and inspects or releases leases.
- `guestctl`: VM-side client, talking to the daemon through the socket smolvm mounts at `/run/clankerd/ctl.sock`.
  Acquires leases and starts browsers.

Vocabulary: *coding agent* (an AI tool in the VM), *lease* (an exclusive named reservation of ports and `.local`
names), *relay* (a TCP forwarder; the VM-side one is `guestctl relay`), *host*, *VM*.

```sh
# host
hostctl smol up|down|status|start|stop
hostctl lease list | show NAME | release NAME [--purge]

# VM
eval "$(guestctl lease acquire shop console.shop.local)"   # CLANKER_LEASE_APP_PORT CLANKER_LEASE_CDP_URL CLANKER_LEASE_HOSTS
guestctl browser start shop                                 # host Chrome, CDP at $CLANKER_LEASE_CDP_URL
guestctl lease show shop | release shop | browser stop shop
```

## Agent skill

The repo is also a Claude Code plugin marketplace (`clankers`) holding the `clankerd` skill, which teaches a coding agent in the VM to use `guestctl`:

```sh
claude plugin marketplace add monai/clankerd
claude plugin install clankerd@clankers
```

## Build

```sh
mise install && mise exec -- make build   # build/{darwin,linux}-arm64/{clankerd,hostctl}, build/linux-arm64/guestctl
mise exec -- make test
```

### Rust workspace (libclankerd, vmctl)

`mise install` pins Rust, zig, cargo-zigbuild and rcodesign. One command builds everything on Linux or macOS:

```sh
mise exec -- make rust   # cross-build, sign, verify -> build/rust/{darwin-arm64/{vmctl,clankerd-vmspawn},linux-arm64/clankerd-guestd}
```

`make rust-lint` and `make rust-test` run clippy/rustfmt and the tests. `clankerd-vmspawn` is ad-hoc signed with the
hypervisor entitlement (`crates/clankerd-vmspawn/entitlements.plist`) and `clankerd-guestd` is a static aarch64 musl
binary; `scripts/rust-verify.sh` checks both.

`scripts/dev-install` builds `linux-arm64/guestctl` and installs it as `/usr/local/bin/guestctl` in the running VM
(the same place the image bakes it). Rerun it after each rebuild; it ends by printing the VM's `guestctl version`.

## Configuration

Precedence, highest first: flags, `CLANKERD_*` environment, project (`.clankerd/config.toml`, found by walking up
from the current directory), user, system, defaults. `--home DIR` / `CLANKERD_HOME` replaces all of these with one directory.

Every key has an environment variable `CLANKERD_<SECTION>_<KEY>` and a flag `--<section>-<key>` (underscores become
hyphens); the table lists the ones that do not follow that pattern. List keys are repeatable flags
(`--smol-volumes=A --smol-volumes=B`) and a JSON array in the environment (`CLANKERD_SMOL_VOLUMES='["A","B"]'`);
`relay_bind` and `mdns.subnets` also take comma-separated values.

| env / flag | TOML | default |
|---|---|---|
| `CLANKERD_VM` `--vm` | `vm.name` | none: required |
| `CLANKERD_SLOTS` | `ports.slots` | `10` |
| `CLANKERD_APP_PORT_BASE` | `ports.app_base` | `4000` |
| `CLANKERD_CDP_PORT_BASE` | `ports.cdp_base` | `9222` |
| `CLANKERD_CHROME_PORT_BASE` | `ports.chrome_base` | `19222` |
| `CLANKERD_RELAY_BIND` (list) | `ports.relay_bind` | `127.0.0.1,::1` |
| `CLANKERD_MDNS_SUBNETS` (list) | `mdns.subnets` | empty: announce nothing |
| `CLANKERD_MDNS_GROUP4` / `GROUP6` | `mdns.group4` / `group6` | `224.0.0.251:5353` / `[ff02::fb]:5353` |
| `CLANKERD_CHROME_BIN` | `chrome.bin` | auto-detect |
| `CLANKERD_HOST_ADDR` | `guest.host_addr` | the VM's default gateways, IPv4 and IPv6 (RFC 8305 Happy Eyeballs) |
| `CLANKERD_GUEST_DIR` | `guest.dir` | `/tmp/clankerd` (relay pidfiles in the VM) |
| `CLANKERD_LOG_LEVEL` | `log.level` | `info` |

`[smol]` (`CLANKERD_SMOL_IMAGE`, `--smol-image` and so on): `image cpus mem storage net net_backend user volumes env init cmd`. None has a default: an unset
key adds no flag, so smolvm's own default applies. `env`, `volumes` and `init` are Go templates with `{{.UID}}` and `{{.GID}}` (the host ids); an unknown field fails `smol up`.
`cmd` is the VM's workload: smolvm launches it on every start, unlike `init`, which runs once on the first.
`contrib/clankers.toml` wires the `ghcr.io/monai/clankers` image; use it with `--config` or `CLANKERD_CONFIG`.
What you configure is what the VM gets: `smol up` and `smol start` on a stopped VM apply the current volumes, ports,
`cpus`, `mem`, `storage`, `net` and `env` to it (`storage` can only grow). `image`, `user`, `init`, `cmd`, `net_backend`
cannot change on an existing VM and fail the command. A running VM is left alone. The applied configuration is
recorded in `<state>/vm-spec`.
The daemon logs to `<state>/clankerd.log`; `clankerd run` runs it in the foreground.
