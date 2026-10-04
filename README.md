# clankerd and clankerctl

Wires coding agents in one smolvm VM to apps, `.local` names and Chrome on the host.

- `clankerd`: host-only daemon, one per VM. Owns all state; answers mDNS, forwards TCP, runs Chrome, drives `smolvm`.
- `clankerctl`: client, on the host and in the VM (through the socket smolvm mounts at `/run/clankerd/ctl.sock`).

Vocabulary: *coding agent* (an AI tool in the VM), *lease* (an exclusive named reservation of ports and `.local`
names), *relay* (a TCP forwarder; the VM-side one is `clankerctl relay`), *host*, *VM*.

```sh
clankerctl smol up|down|status
eval "$(clankerctl lease acquire shop console.shop.local)"   # CLANKER_LEASE_APP_PORT CLANKER_LEASE_CDP_URL CLANKER_LEASE_HOSTS
clankerctl browser start shop                                 # host Chrome, CDP at $CLANKER_LEASE_CDP_URL
clankerctl lease release shop [--purge]
```

`clankerctl` works out which side it is on from the platform: a smolvm guest (`SMOLVM_MACHINE_NAME` on the kernel
command line) or a Docker container (`/.dockerenv`) talks to the daemon through the mounted socket; anything else is the host.

## Build

```sh
mise install && mise exec -- make build   # build/{darwin,linux}-arm64/{clankerd,clankerctl}
mise exec -- make test
```

`scripts/dev-install` builds `linux-arm64/clankerctl` and installs it as `/usr/local/bin/clankerctl` in the running VM
(the same place the image bakes it). Rerun it after each rebuild; it ends by printing the VM's `clankerctl version`.

## Configuration

Precedence, highest first: flags, `CLANKERD_*` environment, project (`.clankerd/config.toml`, found by walking up
from the current directory), user, system, defaults. `--home DIR` / `CLANKERD_HOME` replaces all of these with one directory.

| env / flag | TOML | default |
|---|---|---|
| `CLANKERD_VM` `--vm` | `vm.name` | `sandbox` |
| `CLANKERD_SLOTS` | `ports.slots` | `10` |
| `CLANKERD_APP_PORT_BASE` | `ports.app_base` | `4000` |
| `CLANKERD_CDP_PORT_BASE` | `ports.cdp_base` | `9222` |
| `CLANKERD_CHROME_PORT_BASE` | `ports.chrome_base` | `19222` |
| `CLANKERD_RELAY_BIND` (comma list) | `ports.relay_bind` | `127.0.0.1,::1` |
| `CLANKERD_MDNS_SUBNETS` (comma list) | `mdns.subnets` | empty: announce nothing |
| `CLANKERD_MDNS_GROUP4` / `GROUP6` | `mdns.group4` / `group6` | `224.0.0.251:5353` / `[ff02::fb]:5353` |
| `CLANKERD_CHROME_BIN` | `chrome.bin` | auto-detect |
| `CLANKERD_HOST_ADDR` | `guest.host_addr` | the VM's default gateways, IPv4 and IPv6 (RFC 8305 Happy Eyeballs) |
| `CLANKERD_GUEST_DIR` | `guest.dir` | `/tmp/clankerd` (relay pidfiles in the VM) |
| `CLANKERD_LOG_LEVEL` | `log.level` | `info` |

`[smol]` (TOML only): `image cpus mem storage net net_backend user volumes env init`. None has a default: an unset
key adds no flag, so smolvm's own default applies. `{uid}` and `{gid}` in `env`, `volumes` and `init` expand to the host ids.
`contrib/clankers.toml` wires the `ghcr.io/monai/clankers` image; use it with `--config` or `CLANKERD_CONFIG`.
The daemon logs to `<state>/clankerd.log`; `clankerd run` runs it in the foreground.
