package daemon

import (
	"net"
	"net/netip"
)

// announceAddrs returns every local address inside the subnets. IPv6 link-local addresses are
// skipped: a plain AAAA record cannot carry a zone id.
func announceAddrs(subnets []netip.Prefix) []netip.Addr {
	if len(subnets) == 0 {
		return nil
	}
	addrs, err := net.InterfaceAddrs()
	if err != nil {
		return nil
	}
	var out []netip.Addr
	seen := map[netip.Addr]bool{}
	for _, a := range addrs {
		ipn, ok := a.(*net.IPNet)
		if !ok {
			continue
		}
		ip, ok := netip.AddrFromSlice(ipn.IP)
		if !ok {
			continue
		}
		ip = ip.Unmap()
		if ip.Is6() && ip.IsLinkLocalUnicast() || seen[ip] {
			continue
		}
		for _, s := range subnets {
			if s.Contains(ip) {
				seen[ip] = true
				out = append(out, ip)
				break
			}
		}
	}
	return out
}
