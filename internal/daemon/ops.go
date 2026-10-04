package daemon

import (
	"fmt"
	"net"
	"os"
	"slices"
	"time"

	"github.com/monai/clankers/sandbox/clanker/internal/chrome"
	"github.com/monai/clankers/sandbox/clanker/internal/wire"
)

// handle answers one request. Operations are whitelisted and every argument is validated; nothing a
// client sends is ever run as a command.
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
	d.mu.Lock()
	defer d.mu.Unlock()
	switch req.Op {
	case wire.OpLeaseAcquire:
		return d.acquire(req)
	case wire.OpLeaseRelease:
		return d.release(req)
	case wire.OpLeaseList:
		resp := &wire.Response{Leases: []wire.Lease{}}
		for _, l := range d.sorted() {
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

func (d *Daemon) find(name string) (*lease, error) {
	if err := wire.ValidateName(name); err != nil {
		return nil, err
	}
	l, ok := d.leases[name]
	if !ok {
		return nil, fmt.Errorf("unknown lease %q; run `clankerctl lease acquire %s` first", name, name)
	}
	return l, nil
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
	for _, h := range hosts {
		for _, o := range d.leases {
			if o.Name != req.Name && slices.Contains(o.Hosts, h) {
				return nil, fmt.Errorf("hostname %q is already used by lease %q", h, o.Name)
			}
		}
	}
	if err := d.vmRunning(); err != nil {
		return nil, err
	}

	l, existed := d.leases[req.Name]
	if !existed {
		slot := -1
		for s := 0; s < d.cfg.Slots; s++ {
			if !d.slotUsed(s) {
				slot = s
				break
			}
		}
		if slot < 0 {
			return nil, fmt.Errorf("no free lease: all %d leases are in use (raise ports.slots / CLANKERD_SLOTS to allow more)", d.cfg.Slots)
		}
		l = &lease{}
		l.Name, l.Slot = req.Name, slot
	}
	prev := l.Hosts
	l.Hosts = hosts
	warns, err := d.wire(l)
	if err != nil {
		if existed {
			l.Hosts = prev
		} else if l.cancel != nil {
			l.cancel()
		}
		return nil, err
	}
	d.leases[l.Name] = l
	d.refreshNames()
	if err := d.save(); err != nil {
		return nil, err
	}
	d.log.Info("lease acquired", "name", l.Name, "slot", l.Slot, "hosts", l.Hosts)
	v := d.view(l)
	return &wire.Response{Lease: &v, Warnings: warns}, nil
}

func (d *Daemon) slotUsed(slot int) bool {
	for _, l := range d.leases {
		if l.Slot == slot {
			return true
		}
	}
	return false
}

func (d *Daemon) release(req *wire.Request) (*wire.Response, error) {
	if err := wire.ValidateName(req.Name); err != nil {
		return nil, err
	}
	resp := &wire.Response{}
	l, ok := d.leases[req.Name]
	if !ok {
		resp.Warnings = append(resp.Warnings, fmt.Sprintf("lease %q does not exist", req.Name))
	} else {
		chrome.Stop(l.ChromePID)
		if l.cancel != nil {
			l.cancel()
		}
		d.stopVMRelay(l)
		delete(d.leases, l.Name)
		d.refreshNames()
		if err := d.save(); err != nil {
			return nil, err
		}
		d.log.Info("lease released", "name", l.Name, "slot", l.Slot)
	}
	if req.Purge {
		if err := os.RemoveAll(d.cfg.Dirs.Profile(req.Name)); err != nil {
			return nil, fmt.Errorf("purging profile: %w", err)
		}
	}
	return resp, nil
}

func (d *Daemon) browserStart(req *wire.Request) (*wire.Response, error) {
	l, err := d.find(req.Name)
	if err != nil {
		return nil, err
	}
	if !chrome.Alive(l.ChromePID) {
		bin, err := chrome.Find(d.cfg.ChromeBin)
		if err != nil {
			return nil, err
		}
		pid, err := chrome.Start(bin, d.cfg.Dirs.Profile(l.Name), d.chromePort(l.Slot))
		if err != nil {
			return nil, fmt.Errorf("starting chrome: %w", err)
		}
		l.ChromePID = pid
		if err := d.save(); err != nil {
			return nil, err
		}
		d.log.Info("chrome started", "name", l.Name, "pid", pid)
	}
	v := d.view(l)
	return &wire.Response{Lease: &v}, nil
}

func (d *Daemon) browserStop(req *wire.Request) (*wire.Response, error) {
	if err := wire.ValidateName(req.Name); err != nil {
		return nil, err
	}
	l, ok := d.leases[req.Name]
	if !ok {
		return &wire.Response{Warnings: []string{fmt.Sprintf("lease %q does not exist", req.Name)}}, nil
	}
	chrome.Stop(l.ChromePID)
	l.ChromePID = 0
	if err := d.save(); err != nil {
		return nil, err
	}
	v := d.view(l)
	return &wire.Response{Lease: &v}, nil
}

// resync recreates the VM-side relays and forwarders of every lease, for when the VM was restarted.
func (d *Daemon) resync() (*wire.Response, error) {
	resp := &wire.Response{}
	for _, l := range d.sorted() {
		warns, err := d.wire(l)
		resp.Warnings = append(resp.Warnings, warns...)
		if err != nil {
			resp.Warnings = append(resp.Warnings, fmt.Sprintf("lease %q: %v", l.Name, err))
		}
	}
	return resp, nil
}
