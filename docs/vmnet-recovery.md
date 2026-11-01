# Network loss and recovery

The native VMM owns a stable Unix datagram attachment between libkrun and the bundled native attachment. When the helper exits or its socket becomes unavailable, this attachment discards Ethernet traffic while restarting the helper with the configuration selected at boot. The mode, shared network, physical interface, machine MAC, interface identity and isolation policy stay unchanged. Retries back off to at most 30 seconds; a missing physical interface or failed permission check remains a network outage rather than selecting another mode.

The relay lives in the detached VMM process. Opening or closing vmctl does not own recovery. Inspection marks both families unavailable while the native attachment is disconnected, preserving current guest addresses/routes as configuration details. Guest external results remain observations of the configured numeric endpoints, with their measurement timestamps.

The guest independently supervises its bundled dhcpcd, clears client evidence when it exits, and restarts it with the same configuration. IPv6 peer evidence is rechecked. Address, route, resolver and client-state changes invalidate previous endpoint observations and trigger new probes. Startup still waits for usable networking before launching a workload; subsequent outages leave the workload and vsock management running.

Native verification for this surface remains planned in the [verification skill](../.agents/skills/verify-clankerd/SKILL.md). Run it only on an actual Mac with the required network environment. The lifecycle and bind suites do not establish this behavior. Host sleep/wake, external IPv6 and independent LAN peers require separate observations.
