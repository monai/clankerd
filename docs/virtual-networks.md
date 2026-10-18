Virtual network resources
=========================

Create a persistent resource before assigning machines:

```
vmctl network create --mode shared --subnet 192.168.120.0/24 team
vmctl create --network team --name member IMAGE COMMAND
vmctl network inspect team
vmctl network ls
vmctl stop member
vmctl network create --mode host-only other-team
vmctl update --network other-team member
vmctl start member
vmctl network rm team
```

Modes are `shared` and `host-only`. Omit `--subnet` to allocate an unused /24
from 192.168.64.0/24 through 192.168.254.0/24. Allocation checks overlapping managed resources and persisted machines
under the same state directory. Native network conflicts with other applications
or independent state directories remain native setup failures; there is no
fallback to a different network after creation.

The library exposes `create_network`, `networks`, `inspect_network` and
`remove_network`. `HostConfig.virtual_network` selects membership; inspection
also retains the selected mode and subnet. An assigned resource is authoritative
for these values. Machines select `--network NAME` or `--bridge-interface INTERFACE`; these are mutually exclusive. Mode and subnet belong to the virtual network.

`vmctl update --network NAME MACHINE...` replaces each stopped machine's single
membership. It preserves the workload, volumes and resource settings, selects the
target's mode, subnet and IPv6 prefix, and clears any previous physical bridge
selection. The target must already exist. Running machines are rejected without
being stopped automatically. Changes persist across subsequent starts; there is
no live connect/disconnect or multiple-network membership. Batch updates report
per-machine errors and continue with the remaining machines.

A state-directory flock serializes creation, allocation, membership changes and
network removal across Engine instances and CLI processes. Membership changes
require a stopped machine and its lifecycle lock. A network cannot be removed
while any persisted machine belongs to it, including a stopped machine. Resources
remain until explicitly removed; failed machine creation may leave an allocated
resource for inspection/removal.

A singleton virtual network provides individual machine isolation. Create a
network and assign only that machine; there is no per-machine peer opt-out flag.

Active native attachments register with the bundled native PF controller before
the workload starts. It marks packets by their incoming native bridge and blocks
them from leaving through another managed network's bridge. Members on the same
bridge can communicate. Traffic terminating on the Mac and traffic leaving via
the Mac's physical Internet interface do not cross this boundary. Canonical
host-only members use native host mode without per-interface isolation.

The Mac host and its administrator are trusted. The policy assumes the host
retains the active rules and does not install conflicting rules or states.
Clankerd does not monitor or undo deliberate host firewall changes. To run a
controlled PF experiment, `CLANKERD_NETWORK_PF=off` explicitly disables registration;
this override also disables the production isolation guarantee.

All shared networks persist a ULA /64 in `ipv6_prefix`, including resources
created implicitly for a mode/subnet selection. Member configuration copies it
to `shared_ipv6_prefix`. The bundled attachment requests that prefix and must
acknowledge it before workload startup; restart retains it. Host-only resources
have no global IPv6 prefix.

The bundled `clankerd-network` uses Apple's vmnet APIs and owns PF operations.
Ordinary sudo authorization starts a runtime-owned authorization session; the
VM and Ethernet transport remain unprivileged. No system helper, installer,
sudoers entry or alternate controller is supported. `--network-executable PATH`
(or `CLANKERD_NETWORK`) selects an explicit executable for development.

The VMM-owned relay rejects IPv4 sources outside the selected guest subnet and
IPv6 sources outside the actual native-advertised ULA /64 or link-local scope.
Native metadata must match the selected resource's persisted prefix. Host-only attachments permit link-local IPv6
and its bootstrap traffic, without accepting foreign global IPv6 sources.
Bootstrap DAD, router solicitation and MLD reports remain permitted. These checks
reject foreign-prefix state spoofing before host PF lookup.

The controller keeps a root-owned active-attachment registry under
`/var/run/clankerd-network-pf`, serialized with a file lock across CLI processes
and state directories. It verifies each helper PID, caller, socket and stable virtual-network resource
ID, discovers the bridge through its gateway, validates PF syntax, and replaces only
`com.apple/clankerd-networks`. The normal macOS root rules must include
`anchor "com.apple/*"`. No root ruleset or unrelated states are flushed. Different resource IDs cannot
share one native bridge, even if their subnets and IPv6 prefixes match. One
resource cannot be split over different native bridges. A native addressing
collision across state directories therefore fails startup instead of merging
networks. Each attachment's recovery configuration retains its resource ID.

Stopping an attachment removes its registration. Other members retain the rules;
the last attachment releases clankerd's own PF enable reference. Helper recovery
registers its new PID and bridge before reconnecting Ethernet. Stale helper
registrations are pruned on the next controller operation. Failed registration
prevents workload startup. A failed registry write or rename restores the previous
rules and releases any reference acquired by that registration. A failed reference
release retains a registry entry so cleanup can retry. Failed cleanup is reported and can leave restrictive
rules until the next operation reconciles the registry.

The native-prefix gate now checks the bundled controller with actual routed
IPv4/IPv6 TCP/UDP traffic, same-network controls, Mac access, NAT, member stop,
restart and final cleanup. These checks passed in Mac suite
`/tmp/clankerd-network-gates.M9Cztl`. The host-only gate now additionally verifies
that independent state roots cannot merge distinct resources at the same subnet;
that collision check passed with the lifecycle/recovery fixes in Mac suite
`/tmp/clankerd-network-gates.aI6Qm1`. The stopped-membership update packet
checks also passed in Mac suite `/tmp/clankerd-network-gates.iih6w6` (316s),
including both-direction reassignment, restart persistence and teardown.
Earlier prototype passes do not establish production acceptance. Independent
other-VMM cross-network isolation, sleep/wake, LAN-peer acceptance, host-address
impersonation and additional raw Ethernet cases remain unverified.
