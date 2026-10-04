package cli

import (
	"bufio"
	"encoding/hex"
	"fmt"
	"net/netip"
	"os"
	"strconv"
	"strings"
)

// defaultGateways returns the VM's IPv4 and IPv6 default gateways. Neither family is preferred here;
// the relay's RFC 8305 dialer decides which one to try first.
func defaultGateways() ([]netip.Addr, error) {
	var gws []netip.Addr
	gws = append(gws, scanRoutes("/proc/net/route", func(f []string) (netip.Addr, bool) {
		if len(f) < 3 || f[1] != "00000000" {
			return netip.Addr{}, false
		}
		g, err := strconv.ParseUint(f[2], 16, 32)
		if err != nil || g == 0 {
			return netip.Addr{}, false
		}
		return netip.AddrFrom4([4]byte{byte(g), byte(g >> 8), byte(g >> 16), byte(g >> 24)}), true
	})...)
	gws = append(gws, scanRoutes("/proc/net/ipv6_route", func(f []string) (netip.Addr, bool) {
		// dest destlen src srclen nexthop metric refcnt use flags iface
		if len(f) < 10 || f[0] != strings.Repeat("0", 32) || f[1] != "00" {
			return netip.Addr{}, false
		}
		b, err := hex.DecodeString(f[4])
		if err != nil || len(b) != 16 {
			return netip.Addr{}, false
		}
		a := netip.AddrFrom16([16]byte(b))
		if a.IsUnspecified() {
			return netip.Addr{}, false
		}
		if a.IsLinkLocalUnicast() {
			a = a.WithZone(f[9])
		}
		return a, true
	})...)
	if len(gws) == 0 {
		return nil, fmt.Errorf("no default route; set CLANKERD_HOST_ADDR")
	}
	return gws, nil
}

func scanRoutes(path string, parse func([]string) (netip.Addr, bool)) []netip.Addr {
	f, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer f.Close()
	var out []netip.Addr
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		if a, ok := parse(strings.Fields(sc.Text())); ok {
			out = append(out, a)
		}
	}
	return out
}
