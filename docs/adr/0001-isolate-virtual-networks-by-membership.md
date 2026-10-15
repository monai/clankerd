---
status: accepted
---

# Isolate virtual networks by membership

Machines join virtual networks whose members communicate with each other; a singleton network provides individual isolation, replacing per-machine exclusion inside a common network. This accepts distinct subnets and host PF enforcement between virtual networks: macOS 15 rejected mixed native isolation on one subnet, while packet captures demonstrated routed traffic bypassing native isolation across subnets and controlled PF tests excluded that traffic while preserving the tested host and Internet paths. The decision defines the required production boundary; prototype results do not establish completed lifecycle enforcement, resistance to guest-controlled addressing, or interoperability across isolated networks.
