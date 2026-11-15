# Physical LAN attachment on macOS 15

Create or run with an explicit supported physical interface:

```sh
vmctl run -d --bridge-interface en0 --name lan python:3.13-alpine python3 -m http.server 8080
```

The chosen interface is stored in `host_config.bridge_interface` and reported by
`inspect`. Startup checks vmnet's supported-interface list and the selected
Mac interface's active/up state before launching the workload. A disconnected
or unsupported selection fails instead of switching to another connection.
Change the stopped machine's host configuration to select a different interface.

The native attachment configures Apple's bridged mode with the selected
physical interface. Shared virtual-network selection is rejected for bridged
attachments. DHCPv4, IPv6 RA/SLAAC, and DHCPv6 come from
the physical LAN using the bundled guest client; one usable family suffices.
Use guest addresses directly for TCP/UDP. Users supply any mDNS responder;
clankerd adds no naming service or multicast reflector.

Native verification for this surface remains planned in the [verification skill](../.agents/skills/verify-clankerd/SKILL.md). Run it only on an actual Mac with the required network environment. The lifecycle and bind suites do not establish this behavior. Host sleep/wake, external IPv6 and independent LAN peers require separate observations.
