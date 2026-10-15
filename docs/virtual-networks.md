Virtual network resources
=========================

Create a persistent resource before assigning machines:

```
vmctl network create team --mode shared --subnet 192.168.120.0/24
vmctl create --network team --name member IMAGE COMMAND
vmctl network inspect team
vmctl network ls
vmctl network rm team
```

Modes are `shared` and `host-only`. Omit `--subnet` to allocate an unused /24
from 192.168.64.0/24 through 192.168.254.0/24, reserving 105 and 128 for legacy
defaults. Allocation checks overlapping managed resources and persisted machines
under the same state directory. Native network conflicts with other applications
or independent state directories remain native setup failures; there is no
fallback to a different network after creation.

The library exposes `create_network`, `networks`, `inspect_network` and
`remove_network`. `HostConfig.virtual_network` selects membership; inspection
also retains the selected mode and subnet. An assigned resource is authoritative
for these values. The CLI rejects combining `--network` with legacy mode/subnet/
isolation options. Bridged attachments cannot join a virtual network.

A state-directory flock serializes creation, allocation, membership changes and
network removal across Engine instances and CLI processes. Membership changes
require a stopped machine and its lifecycle lock. A network cannot be removed
while any persisted machine belongs to it, including a stopped machine. Resources
remain until explicitly removed; failed machine creation may leave an allocated
resource for inspection/removal.

Legacy mode/subnet options create or reuse a resource identified by that mode and
subnet. `--isolate-peers` (and API `shared_peer_participation=false`) creates a
singleton network with a separately allocated subnet; it no longer opts a member
out of an existing network. Canonical records set the legacy participation field
to true. Existing records without membership are not silently migrated while
running. They need explicit stopped configuration updates before membership can
serve as an enforced security identity.

This change stores policy identity and addressing. Packet enforcement between
virtual networks requires the host policy integration; subnet selection alone
does not enforce isolation. Native host-only per-interface isolation also needs
replacement to permit peer communication within one host-only virtual network.
