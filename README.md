# clankerd and clankerctl

Wires coding agents in one smolvm VM to apps, `.local` names and Chrome on the host.

- `clankerd`: host-only daemon, one per VM. Owns all state; answers mDNS, forwards TCP, runs Chrome, drives `smolvm`.
- `clankerctl`: client, on the host and in the VM (through the socket smolvm mounts at `/run/clankerd/ctl.sock`).

Vocabulary: *coding agent* (an AI tool in the VM), *lease* (an exclusive named reservation of ports and `.local`
names), *relay* (a TCP forwarder; the VM-side one is `clankerctl relay`), *host*, *VM*.

```sh
clankerctl smol up|down|status
eval "$(clankerctl lease acquire shop console.shop.local)"   # APP_PORT CDP_URL APP_HOSTS
clankerctl browser start shop                                 # host Chrome, CDP at $CDP_URL
clankerctl lease release shop [--purge]
```

## Build

```sh
mise install && mise exec -- make build   # build/{darwin,linux}-arm64/{clankerd,clankerctl}
mise exec -- make test
```

`smol up` links `build/linux-arm64/clankerctl` (mounted at `/mnt/workspace`) into the VM user's `~/.local/bin`,
shadowing any copy baked into the image (`guest.bin` to change or disable).

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
| `CLANKERD_GUEST_BIN` | `guest.bin` | mounted dev build; empty = none |
| `CLANKERD_LOG_LEVEL` | `log.level` | `info` |

`[smol]` (TOML only): `image cpus mem storage net net_backend user volumes env init`.
The daemon logs to `<state>/clankerd.log`; `clankerd run` runs it in the foreground.
