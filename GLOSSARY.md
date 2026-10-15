# Clankerd

Clankerd manages machines and their connectivity. This glossary defines the language used to describe machine networking and its isolation boundaries.

## Language

**Machine**:
A virtual machine managed by clankerd that runs a workload.
_Avoid_: Container when referring to the machine itself.

**Host**:
The Mac on which machines run.
_Avoid_: Server when referring to the host's role in machine networking.

**Guest**:
The operating system and software running inside a machine.

**Virtual network**:
A group of machines whose membership permits peer communication within the group and defines an isolation boundary against other virtual networks.
_Avoid_: Group or domain as the name of the network object; subnet when referring to membership or isolation policy.

**Network membership**:
The relationship between a machine and the virtual network whose peers it is permitted to communicate with.
_Avoid_: Peer participation or peer opt-out for this model.

**Peer**:
Another machine considered in relation to a machine's network connectivity; permitted peers belong to the same virtual network.

**Subnet**:
An IP address range assigned to a network, expressed using an address and prefix length. A subnet describes addressing, not an isolation guarantee.
_Avoid_: IP address class when describing the network's address range.

**Isolation domain**:
The security boundary represented by a virtual network: peer traffic is permitted within it and excluded across its boundary.
_Avoid_: Domain without qualification, which is ambiguous with a DNS domain.

**Singleton network**:
A virtual network containing exactly one machine, providing individual machine isolation through network membership.
_Avoid_: Isolated peer inside a shared group when the machine instead belongs to its own virtual network.

**Shared network**:
A virtual network that supplies built-in Internet access in addition to connectivity with the host and permitted peers.
_Avoid_: Shared as a synonym for unrestricted communication between virtual networks.

**Host-only network**:
A virtual network that provides connectivity with the host without supplying built-in Internet access.
_Avoid_: Offline, which suggests that host connectivity is also absent.

**Bridged attachment**:
A machine's connection to the physical LAN through an explicitly selected host interface, subject to that LAN's connectivity policy.
_Avoid_: Shared network when referring to a physical LAN attachment.

**Gateway**:
The network endpoint through which a guest sends traffic destined beyond its local subnet.
_Avoid_: Physical router when the endpoint belongs to the host's virtual networking.
