// Package relay forwards TCP connections. The daemon uses it for app and CDP forwarders;
// `clankerctl relay` uses it inside the VM. IPv4 and IPv6 are treated alike: listeners are bound
// per address, and outgoing connections follow RFC 8305 (Happy Eyeballs v2).
package relay

import (
	"context"
	"errors"
	"io"
	"net"
	"net/netip"
	"sync"
	"time"
)

// DialFunc opens the outgoing side of one forwarded connection.
type DialFunc func(ctx context.Context) (net.Conn, error)

// Target dials host:port. For a name with several addresses the standard library already applies
// RFC 8305 (address sorting per RFC 6724, interleaved families, staggered attempts).
func Target(hostport string) DialFunc {
	d := net.Dialer{Timeout: 10 * time.Second}
	return func(ctx context.Context) (net.Conn, error) { return d.DialContext(ctx, "tcp", hostport) }
}

// connectionAttemptDelay is RFC 8305 section 5's recommended delay before starting the next attempt.
const connectionAttemptDelay = 250 * time.Millisecond

// HappyEyeballs dials literal addresses per RFC 8305: IPv6 and IPv4 addresses are interleaved, starting
// with IPv6 (section 4); each further attempt starts when the previous one fails or after
// connectionAttemptDelay; the first connection to succeed wins and the others are cancelled (section 5).
// The standard library does this only for names, so for explicit addresses it is done here.
func HappyEyeballs(addrs []netip.Addr, port string) DialFunc {
	var v6, v4, ordered []netip.Addr
	for _, a := range addrs {
		if a.Unmap().Is4() {
			v4 = append(v4, a.Unmap())
		} else {
			v6 = append(v6, a)
		}
	}
	for i := 0; i < len(v6) || i < len(v4); i++ {
		if i < len(v6) {
			ordered = append(ordered, v6[i])
		}
		if i < len(v4) {
			ordered = append(ordered, v4[i])
		}
	}
	return func(ctx context.Context) (net.Conn, error) {
		if len(ordered) == 0 {
			return nil, errors.New("no addresses to dial")
		}
		ctx, cancel := context.WithCancel(ctx)
		defer cancel()
		type result struct {
			c   net.Conn
			err error
		}
		results := make(chan result, len(ordered))
		d := net.Dialer{Timeout: 10 * time.Second}
		start := func(a netip.Addr) {
			go func() {
				c, err := d.DialContext(ctx, "tcp", net.JoinHostPort(a.String(), port))
				results <- result{c, err}
			}()
		}
		var errs []error
		started, finished := 0, 0
		start(ordered[0])
		started++
		timer := time.NewTimer(connectionAttemptDelay)
		defer timer.Stop()
		for finished < started {
			select {
			case r := <-results:
				finished++
				if r.err == nil {
					go func() { // drain the losers
						for ; finished < started; finished++ {
							if l := <-results; l.c != nil {
								l.c.Close()
							}
						}
					}()
					return r.c, nil
				}
				errs = append(errs, r.err)
				if started < len(ordered) { // a failure starts the next attempt at once
					start(ordered[started])
					started++
					timer.Reset(connectionAttemptDelay)
				}
			case <-timer.C:
				if started < len(ordered) {
					start(ordered[started])
					started++
					timer.Reset(connectionAttemptDelay)
				}
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		return nil, errors.Join(errs...)
	}
}

// ListenAll binds one listener per address (IPv4 and IPv6 alike) on port. It fails only if none could
// be bound; the errors of the others are returned for logging.
func ListenAll(addrs []string, port string) ([]net.Listener, []error, error) {
	var ls []net.Listener
	var errs []error
	for _, a := range addrs {
		l, err := net.Listen("tcp", net.JoinHostPort(a, port))
		if err != nil {
			errs = append(errs, err)
			continue
		}
		ls = append(ls, l)
	}
	if len(ls) == 0 {
		return nil, nil, errors.Join(errs...)
	}
	return ls, errs, nil
}

// Serve accepts on l and forwards each connection until l is closed or ctx ends.
func Serve(ctx context.Context, l net.Listener, dial DialFunc) {
	go func() {
		<-ctx.Done()
		l.Close()
	}()
	var wg sync.WaitGroup
	defer wg.Wait()
	for {
		c, err := l.Accept()
		if err != nil {
			return
		}
		wg.Add(1)
		go func() {
			defer wg.Done()
			forward(ctx, c, dial)
		}()
	}
}

func forward(ctx context.Context, in net.Conn, dial DialFunc) {
	defer in.Close()
	out, err := dial(ctx)
	if err != nil {
		return
	}
	defer out.Close()
	stop := context.AfterFunc(ctx, func() { in.Close(); out.Close() })
	defer stop()
	done := make(chan struct{}, 2)
	pipe := func(dst, src net.Conn) {
		io.Copy(dst, src)
		if cw, ok := dst.(interface{ CloseWrite() error }); ok {
			cw.CloseWrite()
		}
		done <- struct{}{}
	}
	go pipe(out, in)
	go pipe(in, out)
	<-done
	<-done
}
