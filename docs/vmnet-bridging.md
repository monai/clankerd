# Physical LAN attachment on macOS 15

Create or run with an explicit supported physical interface:

```sh
vmctl run -d --bridge-interface en0 --name lan python:3.13-alpine python3 -m http.server 8080
```

Find vmnet-supported physical interfaces with the bundled native executable:

```sh
build/artifact/darwin-arm64/clankerd-network authorize
build/artifact/darwin-arm64/clankerd-network launch --list-shared-interfaces
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

Run the real-host gate after building:

```sh
mise exec -- make build && scripts/m2-smoke-bridged.sh
```

The fixture enumerates supported active physical interfaces and explicitly
passes each selected name to the CLI. It tests direct Mac TCP/UDP for available
families, preboot family-filtered startup, persistence/restart, and failure of an
unavailable interface. External probes use family-specific sockets and compare
the same destination from the selected Mac interface and the guest. This does
not establish independent physical-LAN peer reachability, mDNS discovery, or
attachment types absent from that host; those acceptance checks remain separate.
