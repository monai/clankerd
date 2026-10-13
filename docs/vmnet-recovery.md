# Network loss and recovery

The native VMM owns a stable Unix datagram attachment between libkrun and vmnet-helper. When the helper exits or its socket becomes unavailable, this attachment discards Ethernet traffic while restarting the helper with the configuration selected at boot. The mode, shared network, physical interface, machine MAC, interface identity and isolation policy stay unchanged. Retries back off to at most 30 seconds; a missing physical interface or failed permission check remains a network outage rather than selecting another mode.

The relay lives in the detached VMM process. Opening or closing vmctl does not own recovery. Inspection marks both families unavailable while the native attachment is disconnected, preserving current guest addresses/routes as configuration details. Guest external results remain observations of the configured numeric endpoints, with their measurement timestamps.

The guest independently supervises its bundled dhcpcd, clears client evidence when it exits, and restarts it with the same configuration. IPv6 peer evidence is rechecked. Address, route, resolver and client-state changes invalidate previous endpoint observations and trigger new probes. Startup still waits for usable networking before launching a workload; subsequent outages leave the workload and vsock management running.

Run the real macOS 15 recovery gate:

```sh
mise exec -- make rust && scripts/m2-smoke-recovery.sh
```

The gate holds a real guest link/address outage while checking exec, workload output attachment, signals and a host Unix binding. It also kills the configuration client and repeatedly replaces the native helper, then checks direct IPv4 TCP/UDP, unchanged workload identity, unchanged policy and cleanup. Controlled Linux backend tests exercise real Unix datagram packets across failed helper replacement without claiming real vmnet behavior. Host sleep/wake and external IPv6 need separate host evidence.
