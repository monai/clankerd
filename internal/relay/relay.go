// Package relay forwards TCP connections. The daemon uses it for app and CDP forwarders;
// `clankerctl relay` uses it inside the VM.
package relay

import (
	"context"
	"io"
	"net"
	"sync"
	"time"
)

// Serve accepts on l and forwards each connection to target until l is closed or ctx ends.
func Serve(ctx context.Context, l net.Listener, target string) {
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
			forward(ctx, c, target)
		}()
	}
}

func forward(ctx context.Context, in net.Conn, target string) {
	defer in.Close()
	d := net.Dialer{Timeout: 5 * time.Second}
	out, err := d.DialContext(ctx, "tcp", target)
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
