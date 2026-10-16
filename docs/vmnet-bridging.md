# Physical LAN attachment on macOS 15

Create or run with an explicit supported physical interface:

```sh
vmctl run -d --bridge-interface en0 --name lan python:3.13-alpine python3 -m http.server 8080
```

Find vmnet-supported physical interfaces with the installed helper:

```sh
sudo --non-interactive -- /opt/vmnet-helper/bin/vmnet-helper --list-shared-interfaces
```

The chosen interface is stored in `host_config.bridge_interface` and reported by
`inspect`. Startup checks vmnet's supported-interface list and the selected
Mac interface's active/up state before launching the workload. A disconnected
or unsupported selection fails instead of switching to another connection.
Change the stopped machine's host configuration to select a different interface.

The backend uses vmnet-helper's macOS 15 `--operation-mode bridged` and
`--shared-interface` options. It does not pass virtual subnet options or claim
shared-mode peer isolation. Shared network selection and peer isolation options
are rejected for bridged attachments. DHCPv4, IPv6 RA/SLAAC, and DHCPv6 come from
the physical LAN using the bundled guest client; one usable family suffices.
Use guest addresses directly for TCP/UDP. Users supply any mDNS responder;
clankerd adds no naming service or multicast reflector.

Run the real-host gate after building:

```sh
mise exec -- make rust && scripts/m2-smoke-bridged.sh
```

The fixture enumerates supported active physical interfaces and explicitly
passes each selected name to the CLI. It tests direct Mac TCP/UDP for available
families, preboot family-filtered startup, persistence/restart, and failure of an
unavailable interface. External probes use family-specific sockets and compare
the same destination from the selected Mac interface and the guest. This does
not establish independent physical-LAN peer reachability, mDNS discovery, or
attachment types absent from that host; those acceptance checks remain separate.

Primary interface/mode contract: [vmnet-helper v0.13.0 integration](https://github.com/nirs/vmnet-helper/blob/v0.13.0/docs/integration.md#operation-modes).
