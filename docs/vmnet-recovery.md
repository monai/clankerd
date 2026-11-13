# Network loss and recovery

The root `clankerd-network` LaunchDaemon owns all native interfaces. The VMM
holds an XPC connection and a packet socket for its interface. Closing the VMM's
connection releases that interface.

If the daemon fails or is replaced, all running VMs lose networking and require
stop/start. Neither interfaces nor packet sockets reconnect automatically. A new
client can activate the daemon through launchd, but this does not repair existing
VMs. Generation identifiers prevent old VM leases from referring to new interfaces.
Inspection marks both families unavailable when the attachment is lost; vsock
management remains independent.

The guest independently supervises its bundled dhcpcd, clears client evidence when it exits, and restarts it with the same configuration. IPv6 peer evidence is rechecked. Address, route, resolver and client-state changes invalidate previous endpoint observations and trigger new probes. Startup still waits for usable networking before launching a workload; subsequent outages leave the workload and vsock management running.

Native verification for this surface remains planned in the [verification skill](../.agents/skills/verify-clankerd/SKILL.md). Run it only on an actual Mac with the required network environment. The lifecycle and bind suites do not establish this behavior. Host sleep/wake, external IPv6 and independent LAN peers require separate observations.
