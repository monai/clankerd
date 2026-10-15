Host-only native attachment
===========================

`vmctl create/run --network-mode host-only` selects vmnet-helper's `host`
operation mode and always passes `--enable-isolation`. The CLI records peer
participation as false and defaults to `192.168.128.0/24`; `--shared-network`
selects another private subnet. Native host mode supplies no built-in NAT.
The selected mode, subnet and participation policy persist through restart.

API clients set `host_config.network_mode` to `"host-only"` and
`host_config.shared_peer_participation` to `false`. A true participation value
is rejected. API clients should select an unused subnet explicitly: the generic
HostConfig subnet default remains `192.168.105.0/24`. Native host and shared
attachments cannot use the same subnet; setup failures do not fall back to
another network or mode.

This implements native attachment selection, not the complete host-only security
policy. Native isolation alone has not established exclusion of routed traffic
through the Mac or compatible other VMMs. The shared-mode experiments exposed a
routed bypass; production host enforcement is still required. PF integration is
not part of this change. Ticket 05 remains open pending enforcement and actual
Mac IPv4/IPv6 host access, peer exclusion, naming and external-connectivity gates.

Native mode/address behavior:
https://github.com/nirs/vmnet-helper/blob/v0.13.0/docs/integration.md
