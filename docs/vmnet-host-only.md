Host-only virtual networks
==========================

Create a host-only virtual network, then assign its members:

```
vmctl network create local --mode host-only
vmctl run --network local --name first IMAGE COMMAND
vmctl run --network local --name second IMAGE COMMAND
```

Members of `local` are permitted peers. A network containing one machine provides
individual isolation. Native vmnet host mode supplies addressing and Mac access
without built-in NAT; canonical members do not use per-interface native isolation,
which would prevent communication between members of the same network.

Network membership, mode and the allocated subnet persist through machine restart.
The CLI also accepts the legacy `--network-mode host-only` selection, which is
mapped to a virtual network; see [virtual network resources](virtual-networks.md).

Host enforcement between virtual networks is still under implementation. Neither
subnet separation nor absence of a default route establishes exclusion of traffic
routed through the Mac. Ticket 05 remains open until that production boundary and
its lifecycle pass actual Mac packet tests.
