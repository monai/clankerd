// Package mdns is a respond-only multicast DNS responder: it answers A and AAAA questions for the
// names it is told about, with the addresses it is told about. No probing, no announcements.
package mdns

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"net/netip"
	"strings"
	"sync"
	"syscall"

	"github.com/miekg/dns"
	"golang.org/x/net/ipv4"
	"golang.org/x/net/ipv6"
	"golang.org/x/sys/unix"
)

const (
	ttl        = 120
	cacheFlush = 0x8000
	mdnsPort   = 5353
)

type Options struct {
	Group4 string // "224.0.0.251:5353"; a non-multicast address is bound as a plain unicast socket (tests)
	Group6 string // "[ff02::fb]:5353"
	// Known reports whether name (lowercase, no trailing dot) is registered.
	Known func(name string) bool
	// Addrs returns the addresses to answer with, right now.
	Addrs func() []netip.Addr
	Log   *slog.Logger
}

// Start binds the sockets and answers queries until ctx ends. It fails only if no socket could be bound.
func Start(ctx context.Context, o Options) error {
	if o.Log == nil {
		o.Log = slog.Default()
	}
	var started int
	for _, family := range []struct {
		network, group string
	}{{"udp4", o.Group4}, {"udp6", o.Group6}} {
		if family.group == "" {
			continue
		}
		s, err := newSocket(ctx, family.network, family.group, o)
		if err != nil {
			o.Log.Warn("mdns socket unavailable", "network", family.network, "group", family.group, "err", err)
			continue
		}
		started++
		go s.serve(ctx)
	}
	if started == 0 {
		return fmt.Errorf("could not bind any mDNS socket")
	}
	return nil
}

type socket struct {
	o      Options
	conn   net.PacketConn
	group  *net.UDPAddr
	v6     bool
	mcast  bool
	ifaces []net.Interface
	mu     sync.Mutex // serialises multicast-interface selection and writes
	p4     *ipv4.PacketConn
	p6     *ipv6.PacketConn
}

func reusable(ctx context.Context, network, addr string) (net.PacketConn, error) {
	lc := net.ListenConfig{Control: func(network, _ string, c syscall.RawConn) error {
		var serr error
		err := c.Control(func(fd uintptr) {
			if serr = unix.SetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_REUSEADDR, 1); serr != nil {
				return
			}
			if serr = unix.SetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_REUSEPORT, 1); serr != nil {
				return
			}
			if network == "udp6" { // keep the IPv4 socket's traffic out of the IPv6 one
				serr = unix.SetsockoptInt(int(fd), unix.IPPROTO_IPV6, unix.IPV6_V6ONLY, 1)
			}
		})
		if err != nil {
			return err
		}
		return serr
	}}
	return lc.ListenPacket(ctx, network, addr)
}

func newSocket(ctx context.Context, network, group string, o Options) (*socket, error) {
	ga, err := net.ResolveUDPAddr(network, group)
	if err != nil {
		return nil, err
	}
	s := &socket{o: o, group: ga, v6: network == "udp6", mcast: ga.IP.IsMulticast()}
	bind := group
	if s.mcast {
		if s.v6 {
			bind = fmt.Sprintf("[::]:%d", ga.Port)
		} else {
			bind = fmt.Sprintf("0.0.0.0:%d", ga.Port)
		}
	}
	if s.conn, err = reusable(ctx, network, bind); err != nil {
		return nil, err
	}
	if s.mcast {
		if err := s.join(); err != nil {
			s.conn.Close()
			return nil, err
		}
	}
	return s, nil
}

func (s *socket) join() error {
	all, err := net.Interfaces()
	if err != nil {
		return err
	}
	if s.v6 {
		s.p6 = ipv6.NewPacketConn(s.conn)
		s.p6.SetMulticastHopLimit(255)
	} else {
		s.p4 = ipv4.NewPacketConn(s.conn)
		s.p4.SetMulticastTTL(255)
	}
	for _, ifi := range all {
		if ifi.Flags&net.FlagUp == 0 || ifi.Flags&net.FlagMulticast == 0 || ifi.Flags&net.FlagLoopback != 0 {
			continue
		}
		var jerr error
		if s.v6 {
			jerr = s.p6.JoinGroup(&ifi, &net.UDPAddr{IP: s.group.IP})
		} else {
			jerr = s.p4.JoinGroup(&ifi, &net.UDPAddr{IP: s.group.IP})
		}
		if jerr != nil {
			s.o.Log.Debug("mdns join failed", "iface", ifi.Name, "err", jerr)
			continue
		}
		s.ifaces = append(s.ifaces, ifi)
	}
	if len(s.ifaces) == 0 {
		return fmt.Errorf("could not join %s on any interface", s.group.IP)
	}
	return nil
}

func (s *socket) serve(ctx context.Context) {
	go func() {
		<-ctx.Done()
		s.conn.Close()
	}()
	buf := make([]byte, 9000)
	for {
		n, src, err := s.conn.ReadFrom(buf)
		if err != nil {
			return
		}
		var q dns.Msg
		if q.Unpack(buf[:n]) != nil || q.Response || len(q.Question) == 0 {
			continue
		}
		s.answer(&q, src)
	}
}

func (s *socket) answer(q *dns.Msg, src net.Addr) {
	var asked struct{ a, aaaa bool }
	var names []string
	for _, qu := range q.Question {
		if qu.Qclass&^cacheFlush != dns.ClassINET && qu.Qclass != dns.ClassANY {
			continue
		}
		name := strings.ToLower(strings.TrimSuffix(qu.Name, "."))
		if !s.o.Known(name) {
			continue
		}
		switch qu.Qtype {
		case dns.TypeA:
			asked.a = true
		case dns.TypeAAAA:
			asked.aaaa = true
		case dns.TypeANY:
			asked.a, asked.aaaa = true, true
		default:
			continue
		}
		names = append(names, qu.Name)
	}
	if len(names) == 0 {
		return
	}
	addrs := s.o.Addrs()
	build := func(class uint16) []dns.RR {
		var rrs []dns.RR
		seen := map[string]bool{}
		for _, n := range names {
			for _, a := range addrs {
				a = a.Unmap()
				key := n + "|" + a.String()
				if seen[key] {
					continue
				}
				hdr := dns.RR_Header{Name: n, Class: class, Ttl: ttl}
				if a.Is4() && asked.a {
					hdr.Rrtype = dns.TypeA
					rrs = append(rrs, &dns.A{Hdr: hdr, A: net.IP(a.AsSlice())})
				} else if a.Is6() && asked.aaaa {
					hdr.Rrtype = dns.TypeAAAA
					rrs = append(rrs, &dns.AAAA{Hdr: hdr, AAAA: net.IP(a.AsSlice())})
				} else {
					continue
				}
				seen[key] = true
			}
		}
		return rrs
	}

	// Unicast to the asker. A legacy (non-5353) asker gets its question echoed and no cache-flush bit.
	legacy := true
	if ua, ok := src.(*net.UDPAddr); ok {
		legacy = ua.Port != mdnsPort
	}
	class := uint16(dns.ClassINET | cacheFlush)
	if legacy {
		class = dns.ClassINET
	}
	uni := &dns.Msg{}
	uni.Response, uni.Authoritative = true, true
	uni.Answer = build(class)
	if len(uni.Answer) == 0 {
		return
	}
	if legacy {
		uni.Id = q.Id
		uni.Question = q.Question
	}
	if wire, err := uni.Pack(); err == nil {
		s.mu.Lock()
		_, err = s.conn.WriteTo(wire, src)
		s.mu.Unlock()
		if err != nil {
			s.o.Log.Debug("mdns unicast reply failed", "err", err)
		}
	}

	// Multicast to the group, on every joined interface.
	mc := &dns.Msg{}
	mc.Response, mc.Authoritative = true, true
	mc.Answer = build(dns.ClassINET | cacheFlush)
	wire, err := mc.Pack()
	if err != nil {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if !s.mcast {
		s.conn.WriteTo(wire, s.group)
		return
	}
	for i := range s.ifaces {
		if s.v6 {
			s.p6.SetMulticastInterface(&s.ifaces[i])
			s.p6.WriteTo(wire, nil, s.group)
		} else {
			s.p4.SetMulticastInterface(&s.ifaces[i])
			s.p4.WriteTo(wire, nil, s.group)
		}
	}
}
