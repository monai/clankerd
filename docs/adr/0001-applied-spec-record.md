# Record the applied VM spec in clankerctl

`smol up`/`smol start` reconcile a stopped VM to the current config through `smolvm machine update`. smolvm
exposes only a mount count, not the volume list, so clankerctl records the full resolved spec as JSON in
`<state>/vm-spec` and diffs against it. The record is written only after `machine update` succeeds. A running VM is
never compared or touched. A record that is not JSON (the hash older versions wrote) counts as drifted and needs one
`smol down` / `smol up`.
