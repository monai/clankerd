Host-only virtual networks
==========================

Create a host-only virtual network, then assign its members:

```
vmctl network create --mode host-only local
vmctl run --network local --name first IMAGE COMMAND
vmctl run --network local --name second IMAGE COMMAND
```

Members of `local` are permitted peers. A network containing one machine provides
individual isolation. Native vmnet host mode supplies addressing and Mac access
without built-in NAT; canonical members do not use per-interface native isolation,
which would prevent communication between members of the same network.

Network membership, mode and the allocated subnet persist through machine restart.
Use `--network NAME` to select membership; mode and subnet are defined on the resource.

The helper enforces isolation between virtual networks through PF. See
[virtual networks](virtual-networks.md) for the policy and validation limits.
