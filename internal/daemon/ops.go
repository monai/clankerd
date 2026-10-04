package daemon

import (
	"fmt"
	"net"
	"os"
	"slices"
	"sync"
	"time"

	"github.com/monai/clankers/sandbox/clankerd/internal/chrome"
	"github.com/monai/clankers/sandbox/clankerd/internal/state"
	"github.com/monai/clankers/sandbox/clankerd/internal/wire"
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
			"protocol version mismatch: client speaks %d, clankerd speaks %d; run the same build on host and VM "+
				"(`clankerctl smol up` relinks the VM's copy)", req.V, wire.Version)})
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
	return fmt.Errorf("unknown lease %q; run `clankerctl lease acquire %s` first", name, name)
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
	if _, ready := l.snapshot(); !ready {
		return nil, errUnknown(name)
	}
	return l, nil
}

func (d *Daemon) lock(name string) (*lease, error) {
	l, err := d.find(name)
	if err != nil {
		return nil, err
	}
	l.op.Lock()
	if l.dead {
		l.op.Unlock()
		return nil, errUnknown(name)
	}
	return l, nil
}

func (d *Daemon) reserve(name string, hosts []string) (l *lease, created bool, err error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	for _, h := range hosts {
		if owner, ok := d.claims[h]; ok && owner != name {
			return nil, false, fmt.Errorf("hostname %q is already used by lease %q", h, owner)
		}
	}
	l, ok := d.leases[name]
	if !ok {
		used := map[int]bool{}
		for _, o := range d.leases {
			used[o.slot()] = true
		}
		slot := -1
		for s := 0; s < d.cfg.Slots; s++ {
			if !used[s] {
				slot = s
				break
			}
		}
		if slot < 0 {
			return nil, false, fmt.Errorf("no free lease: all %d leases are in use (raise ports.slots / CLANKERD_SLOTS to allow more)", d.cfg.Slots)
		}
		l = &lease{state: state.Lease{Name: name, Slot: slot}}
		l.op.Lock()
		d.leases[name] = l
		created = true
	}
	for _, h := range hosts {
		d.claims[h] = name
	}
	return l, created, nil
}

func (d *Daemon) setClaims(name string, hosts []string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	for h, owner := range d.claims {
		if owner == name {
			delete(d.claims, h)
		}
	}
	for _, h := range hosts {
		d.claims[h] = name
	}
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

	var l *lease
	for {
		var created bool
		var err error
		l, created, err = d.reserve(req.Name, hosts)
		if err != nil {
			return nil, err
		}
		if created {
			break
		}
		l.op.Lock()
		if !l.dead {
			break
		}
		l.op.Unlock()
	}
	defer l.op.Unlock()

	prev, wasReady := l.snapshot()
	warns, err := d.ensureLease(l)
	if err != nil {
		if wasReady {
			d.setClaims(l.name(), prev.Hosts)
		} else {
			d.drop(l)
		}
		return nil, err
	}
	l.update(func(s *state.Lease) { s.Hosts = hosts })
	l.mu.Lock()
	l.ready = true
	l.mu.Unlock()
	d.setClaims(l.name(), hosts)
	d.refreshNames()
	if err := d.save(); err != nil {
		return nil, err
	}
	d.log.Info("lease acquired", "name", l.name(), "slot", l.slot(), "hosts", hosts)
	v := d.view(l)
	return &wire.Response{Lease: &v, Warnings: warns}, nil
}

func (d *Daemon) drop(l *lease) {
	l.stopLocal()
	l.dead = true
	d.mu.Lock()
	delete(d.leases, l.name())
	d.mu.Unlock()
	d.setClaims(l.name(), nil)
}

func (d *Daemon) release(req *wire.Request) (*wire.Response, error) {
	if err := wire.ValidateName(req.Name); err != nil {
		return nil, err
	}
	resp := &wire.Response{}
	d.mu.Lock()
	l := d.leases[req.Name]
	d.mu.Unlock()
	if l != nil {
		l.op.Lock()
		if l.dead {
			l = nil
		}
	}
	if l == nil {
		resp.Warnings = append(resp.Warnings, fmt.Sprintf("lease %q does not exist", req.Name))
	} else {
		s, _ := l.snapshot()
		chrome.Stop(s.ChromePID, d.profile(l.name()))
		l.stopLocal()
		d.stopVMRelay(l)
		d.drop(l)
		l.op.Unlock()
		d.refreshNames()
		if err := d.save(); err != nil {
			return nil, err
		}
		d.log.Info("lease released", "name", req.Name, "slot", l.slot())
	}
	if req.Purge {
		if err := os.RemoveAll(d.profile(req.Name)); err != nil {
			return nil, fmt.Errorf("purging profile: %w", err)
		}
	}
	return resp, nil
}

func (d *Daemon) browserStart(req *wire.Request) (*wire.Response, error) {
	l, err := d.lock(req.Name)
	if err != nil {
		return nil, err
	}
	defer l.op.Unlock()
	s, _ := l.snapshot()
	if !chrome.Running(s.ChromePID, d.profile(l.name())) {
		bin, err := chrome.Find(d.cfg.ChromeBin)
		if err != nil {
			return nil, err
		}
		pid, err := chrome.Start(bin, d.profile(l.name()), d.chromePort(l.slot()))
		if err != nil {
			return nil, fmt.Errorf("starting chrome: %w", err)
		}
		l.update(func(s *state.Lease) { s.ChromePID = pid })
		if err := d.save(); err != nil {
			return nil, err
		}
		d.log.Info("chrome started", "name", l.name(), "pid", pid)
	}
	v := d.view(l)
	return &wire.Response{Lease: &v}, nil
}

func (d *Daemon) browserStop(req *wire.Request) (*wire.Response, error) {
	if err := wire.ValidateName(req.Name); err != nil {
		return nil, err
	}
	l, err := d.lock(req.Name)
	if err != nil {
		return &wire.Response{Warnings: []string{fmt.Sprintf("lease %q does not exist", req.Name)}}, nil
	}
	defer l.op.Unlock()
	s, _ := l.snapshot()
	chrome.Stop(s.ChromePID, d.profile(l.name()))
	l.update(func(s *state.Lease) { s.ChromePID = 0 })
	if err := d.save(); err != nil {
		return nil, err
	}
	v := d.view(l)
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
			l.op.Lock()
			defer l.op.Unlock()
			if l.dead {
				return
			}
			warns, err := d.ensureLease(l)
			mu.Lock()
			defer mu.Unlock()
			resp.Warnings = append(resp.Warnings, warns...)
			if err != nil {
				resp.Warnings = append(resp.Warnings, fmt.Sprintf("lease %q: %v", l.name(), err))
			}
		}()
	}
	wg.Wait()
	return resp, nil
}
