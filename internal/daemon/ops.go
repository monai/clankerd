package daemon

import (
	"fmt"
	"net"
	"os"
	"slices"
	"sync"
	"time"

	"github.com/monai/clankers/clankerd/internal/chrome"
	"github.com/monai/clankers/clankerd/internal/wire"
)

func (d *Daemon) handle(c net.Conn) {
	defer c.Close()
	c.SetDeadline(time.Now().Add(90 * time.Second))
	req, err := wire.ReadRequest(c)
	if err != nil {
		wire.WriteResponse(c, &wire.Response{Error: err.Error()})
		return
	}
	if req.V != wire.Version {
		wire.WriteResponse(c, &wire.Response{Error: fmt.Sprintf(
			"protocol version mismatch: client speaks %d, clankerd speaks %d; run the same build on host and VM", req.V, wire.Version)})
		return
	}
	resp, err := d.dispatch(req)
	if err != nil {
		d.log.Info("request failed", "op", req.Op, "name", req.Name, "err", err)
		resp = &wire.Response{Error: err.Error()}
	}
	resp.OK = err == nil
	wire.WriteResponse(c, resp)
}

func (d *Daemon) dispatch(req *wire.Request) (*wire.Response, error) {
	switch req.Op {
	case wire.OpLeaseAcquire:
		return d.acquire(req)
	case wire.OpLeaseRelease:
		return d.release(req)
	case wire.OpLeaseList:
		resp := &wire.Response{Leases: []wire.Lease{}}
		for _, l := range d.readyLeases() {
			resp.Leases = append(resp.Leases, d.view(l))
		}
		return resp, nil
	case wire.OpLeaseShow:
		l, err := d.find(req.Name)
		if err != nil {
			return nil, err
		}
		v := d.view(l)
		return &wire.Response{Lease: &v}, nil
	case wire.OpBrowserStart:
		return d.browserStart(req)
	case wire.OpBrowserStop:
		return d.browserStop(req)
	case wire.OpResync:
		return d.resync()
	}
	return nil, fmt.Errorf("unknown operation %q", req.Op)
}

func errUnknown(name string) error {
	return fmt.Errorf("unknown lease %q; run `guestctl lease acquire %s` first", name, name)
}

func (d *Daemon) find(name string) (*lease, error) {
	if err := wire.ValidateName(name); err != nil {
		return nil, err
	}
	d.mu.Lock()
	l := d.leases[name]
	d.mu.Unlock()
	if l == nil {
		return nil, errUnknown(name)
	}
	if _, p := l.snapshot(); p != active {
		return nil, errUnknown(name)
	}
	return l, nil
}

func (d *Daemon) lock(name string) (*held, error) {
	l, err := d.find(name)
	if err != nil {
		return nil, err
	}
	h := l.lock()
	if h.gone() {
		h.unlock()
		return nil, errUnknown(name)
	}
	return h, nil
}

func (d *Daemon) reserve(name string, hosts []string) (h *held, created bool, err error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if err := d.claims.claim(name, hosts); err != nil {
		return nil, false, err
	}
	if l, ok := d.leases[name]; ok {
		return &held{l}, false, nil
	}
	used := map[int]bool{}
	for _, o := range d.leases {
		used[o.slot] = true
	}
	for slot := 0; slot < d.cfg.Slots; slot++ {
		if !used[slot] {
			l := &lease{name: name, slot: slot}
			d.leases[name] = l
			return l.lock(), true, nil
		}
	}
	d.claims.set(name, nil)
	return nil, false, fmt.Errorf("no free lease: all %d leases are in use (raise ports.slots / CLANKERD_SLOTS to allow more)", d.cfg.Slots)
}

func (d *Daemon) acquire(req *wire.Request) (*wire.Response, error) {
	if err := wire.ValidateName(req.Name); err != nil {
		return nil, err
	}
	hosts := []string{req.Name + ".local"}
	for _, h := range req.Hosts {
		if err := wire.ValidateHost(h); err != nil {
			return nil, err
		}
		if !slices.Contains(hosts, h) {
			hosts = append(hosts, h)
		}
	}
	if err := d.vmRunning(); err != nil {
		return nil, err
	}

	var h *held
	for {
		r, created, err := d.reserve(req.Name, hosts)
		if err != nil {
			return nil, err
		}
		if created {
			h = r
			break
		}
		h = r.lease.lock()
		if !h.gone() {
			break
		}
		h.unlock()
	}
	defer h.unlock()

	prev, ph := h.snapshot()
	wasActive := ph == active
	warns, err := d.ensureLease(h)
	if err != nil {
		if wasActive {
			d.claims.set(h.name, prev.Hosts)
		} else {
			d.drop(h)
		}
		return nil, err
	}
	h.setHosts(hosts)
	h.setPhase(active)
	d.claims.set(h.name, hosts)
	d.refreshNames()
	if err := d.save(); err != nil {
		return nil, err
	}
	d.log.Info("lease acquired", "name", h.name, "slot", h.slot, "hosts", hosts)
	v := d.view(h.lease)
	return &wire.Response{Lease: &v, Warnings: warns}, nil
}

func (d *Daemon) drop(h *held) {
	h.run.close()
	h.setPhase(released)
	d.mu.Lock()
	delete(d.leases, h.name)
	d.mu.Unlock()
	d.claims.set(h.name, nil)
}

func (d *Daemon) release(req *wire.Request) (*wire.Response, error) {
	if err := wire.ValidateName(req.Name); err != nil {
		return nil, err
	}
	resp := &wire.Response{}
	d.mu.Lock()
	l := d.leases[req.Name]
	d.mu.Unlock()
	var h *held
	if l != nil {
		if h = l.lock(); h.gone() {
			h.unlock()
			h = nil
		}
	}
	if h == nil {
		resp.Warnings = append(resp.Warnings, fmt.Sprintf("lease %q does not exist", req.Name))
	} else {
		s, _ := h.snapshot()
		chrome.Stop(s.ChromePID, d.profile(h.name))
		h.run.close()
		d.stopVMRelay(h.lease)
		d.drop(h)
		h.unlock()
		d.refreshNames()
		if err := d.save(); err != nil {
			return nil, err
		}
		d.log.Info("lease released", "name", req.Name, "slot", h.slot)
	}
	if req.Purge {
		if err := os.RemoveAll(d.profile(req.Name)); err != nil {
			return nil, fmt.Errorf("purging profile: %w", err)
		}
	}
	return resp, nil
}

func (d *Daemon) browserStart(req *wire.Request) (*wire.Response, error) {
	h, err := d.lock(req.Name)
	if err != nil {
		return nil, err
	}
	defer h.unlock()
	s, _ := h.snapshot()
	if !chrome.Running(s.ChromePID, d.profile(h.name)) {
		bin, err := chrome.Find(d.cfg.ChromeBin)
		if err != nil {
			return nil, err
		}
		pid, err := chrome.Start(bin, d.profile(h.name), d.chromePort(h.slot))
		if err != nil {
			return nil, fmt.Errorf("starting chrome: %w", err)
		}
		h.setChromePID(pid)
		if err := d.save(); err != nil {
			return nil, err
		}
		d.log.Info("chrome started", "name", h.name, "pid", pid)
	}
	v := d.view(h.lease)
	return &wire.Response{Lease: &v}, nil
}

func (d *Daemon) browserStop(req *wire.Request) (*wire.Response, error) {
	if err := wire.ValidateName(req.Name); err != nil {
		return nil, err
	}
	h, err := d.lock(req.Name)
	if err != nil {
		return &wire.Response{Warnings: []string{fmt.Sprintf("lease %q does not exist", req.Name)}}, nil
	}
	defer h.unlock()
	s, _ := h.snapshot()
	chrome.Stop(s.ChromePID, d.profile(h.name))
	h.setChromePID(0)
	if err := d.save(); err != nil {
		return nil, err
	}
	v := d.view(h.lease)
	return &wire.Response{Lease: &v}, nil
}

func (d *Daemon) resync() (*wire.Response, error) {
	var (
		wg   sync.WaitGroup
		mu   sync.Mutex
		resp = &wire.Response{}
	)
	for _, l := range d.readyLeases() {
		wg.Add(1)
		go func() {
			defer wg.Done()
			h := l.lock()
			defer h.unlock()
			if h.gone() {
				return
			}
			warns, err := d.ensureLease(h)
			mu.Lock()
			defer mu.Unlock()
			resp.Warnings = append(resp.Warnings, warns...)
			if err != nil {
				resp.Warnings = append(resp.Warnings, fmt.Sprintf("lease %q: %v", l.name, err))
			}
		}()
	}
	wg.Wait()
	return resp, nil
}
