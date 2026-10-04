package daemon

import (
	"context"
	"fmt"
	"net"
	"net/netip"
	"sync"

	"github.com/monai/clankers/sandbox/clankerd/internal/state"
)

type phase int

const (
	acquiring phase = iota
	active
	released
)

type lease struct {
	name string
	slot int

	op  sync.Mutex
	run running

	mu        sync.Mutex
	hosts     []string
	chromePID int
	phase     phase
}

type running struct {
	ctx        context.Context
	cancel     context.CancelFunc
	listeners  []net.Listener
	forwarders map[netip.Addr]forwarder
}

type forwarder struct {
	cancel context.CancelFunc
	ln     net.Listener
}

func (r *running) close() {
	if r.cancel != nil {
		r.cancel()
	}
	for _, ln := range r.listeners {
		ln.Close()
	}
	for _, f := range r.forwarders {
		f.cancel()
		f.ln.Close()
	}
}

type held struct{ *lease }

func (l *lease) lock() *held {
	l.op.Lock()
	return &held{l}
}

func (h *held) unlock() { h.op.Unlock() }

func (h *held) gone() bool {
	_, p := h.snapshot()
	return p == released
}

func (l *lease) snapshot() (state.Lease, phase) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return state.Lease{Name: l.name, Slot: l.slot, Hosts: append([]string(nil), l.hosts...), ChromePID: l.chromePID}, l.phase
}

func (l *lease) setHosts(hosts []string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.hosts = hosts
}

func (l *lease) setChromePID(pid int) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.chromePID = pid
}

func (l *lease) setPhase(p phase) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.phase = p
}

type claims struct {
	mu    sync.Mutex
	owner map[string]string
}

func newClaims() *claims { return &claims{owner: map[string]string{}} }

func (c *claims) claim(name string, hosts []string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, h := range hosts {
		if o, ok := c.owner[h]; ok && o != name {
			return fmt.Errorf("hostname %q is already used by lease %q", h, o)
		}
	}
	for _, h := range hosts {
		c.owner[h] = name
	}
	return nil
}

func (c *claims) set(name string, hosts []string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for h, o := range c.owner {
		if o == name {
			delete(c.owner, h)
		}
	}
	for _, h := range hosts {
		c.owner[h] = name
	}
}
