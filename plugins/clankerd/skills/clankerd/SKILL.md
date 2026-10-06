---
name: clankerd
description: How to use `guestctl` in the VM to lease ports and `.local` names and drive the host's Chrome.
disable-model-invocation: true
---

# clankerd

You are in a VM. `guestctl` talks to the host daemon. A **lease** is a named set of one app port, one Chrome (CDP) port and `.local` names. Use one lease per app, like `shop`.

## Steps

1. Get the lease:

   ```sh
   eval "$(guestctl lease acquire shop console.shop.local)"
   ```

   This sets `CLANKER_LEASE_APP_PORT`, `CLANKER_LEASE_CDP_URL` and `CLANKER_LEASE_HOSTS`. `shop.local` is always added. Extra names are optional.

2. Run your app on `0.0.0.0:$CLANKER_LEASE_APP_PORT`. The host opens it at `http://shop.local:$CLANKER_LEASE_APP_PORT`.

3. Start the host's Chrome:

   ```sh
   guestctl browser start shop
   ```

   Point your tool (Playwright, Puppeteer) at `$CLANKER_LEASE_CDP_URL`. Done when `curl "$CLANKER_LEASE_CDP_URL/json/version"` answers.

4. When finished: `guestctl browser stop shop`.

## Commands

| Command | Does |
| :- | :- |
| `lease acquire NAME [HOST.local ...]` | Reserve or update a lease. Safe to repeat. New hostnames replace the old ones. |
| `lease show NAME` | Print the lease again. |
| `lease release NAME [--purge]` | Free the lease. `--purge` also deletes its Chrome profile. |
| `browser start NAME` | Start Chrome. OK if already running. |
| `browser stop NAME` | Stop Chrome. OK if already stopped. |

Add `--json` to `lease acquire` or `lease show` for JSON output.

`NAME`: 1-40 chars of `a-z 0-9 -`. Hostnames: lowercase, end in `.local`.

## Errors

| Message has | Do this |
| :- | :- |
| `control socket … is missing` | Ask the user to run `hostctl smol down`, then `hostctl smol up`. |
| `clankerd is not running` | Ask the user to run `hostctl smol up`. |
| `no free lease` | Reuse a lease name, or release one you no longer need. |
| `already used by lease` | Pick another hostname. |
| `unknown lease` | Run `guestctl lease acquire NAME` first. |
