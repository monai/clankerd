// Package daemon is clankerd: it owns every lease and does the stateful work for it.
package daemon

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"net/netip"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/monai/clankers/sandbox/clanker/internal/backend"
	"github.com/monai/clankers/sandbox/clanker/internal/chrome"
	"github.com/monai/clankers/sandbox/clanker/internal/config"
	"github.com/monai/clankers/sandbox/clanker/internal/mdns"
	"github.com/monai/clankers/sandbox/clanker/internal/relay"
	"github.com/monai/clankers/sandbox/clanker/internal/state"
	"github.com/monai/clankers/sandbox/clanker/internal/wire"
)

const execTimeout = 30 * time.Second

type lease struct {
	state.Lease
	ctx        context.Context    // ends when the lease is released or the daemon stops
	cancel     context.CancelFunc // stops the lease's daemon-side relays and forwarders
	forwarders map[netip.Addr]context.CancelFunc
}

type Daemon struct {
	cfg *config.Config
	vm  backend.Backend
	log *slog.Logger

	ctx    context.Context
	mu     sync.Mutex // guards leases and all work on them
	leases map[string]*lease

	namesMu sync.RWMutex
	names   map[string]bool
}

func New(ctx context.Context, cfg *config.Config, vm backend.Backend, log *slog.Logger) *Daemon {
	return &Daemon{ctx: ctx, cfg: cfg, vm: vm, log: log, leases: map[string]*lease{}, names: map[string]bool{}}
}

func (d *Daemon) appPort(slot int) int    { return d.cfg.AppPortBase + slot }
func (d *Daemon) cdpPort(slot int) int    { return d.cfg.CDPPortBase + slot }
func (d *Daemon) chromePort(slot int) int { return d.cfg.ChromePortBase + slot }

func (d *Daemon) pidFile(name string) string {
	return filepath.Join(d.cfg.GuestDir, "relay-"+name+".pid")
}

// Serve runs the daemon until ctx ends: it takes the pidfile and socket, restores state, and answers.
func (d *Daemon) Serve(ctx context.Context) error {
	dirs := d.cfg.Dirs
	for _, p := range []string{dirs.Runtime, dirs.State} {
		if err := os.MkdirAll(p, 0o700); err != nil {
			return err
		}
	}
	if err := d.takePIDFile(); err != nil {
		return err
	}
	defer os.Remove(dirs.PIDFile())

	ln, err := d.listen()
	if err != nil {
		return err
	}
	defer os.Remove(dirs.Socket())

	if err := mdns.Start(ctx, mdns.Options{
		Group4: d.cfg.MDNSGroup4, Group6: d.cfg.MDNSGroup6, Log: d.log,
		Known: func(n string) bool { d.namesMu.RLock(); defer d.namesMu.RUnlock(); return d.names[n] },
		Addrs: func() []netip.Addr { return announceAddrs(d.cfg.MDNSSubnets) },
	}); err != nil {
		d.log.Warn("mdns disabled", "err", err)
	}

	if err := d.restore(); err != nil {
		return err
	}

	go func() {
		<-ctx.Done()
		ln.Close()
	}()
	d.log.Info("clankerd listening", "socket", dirs.Socket(), "vm", d.cfg.VM)
	var wg sync.WaitGroup
	for {
		c, err := ln.Accept()
		if err != nil {
			break
		}
		wg.Add(1)
		go func() {
			defer wg.Done()
			d.handle(c)
		}()
	}
	wg.Wait()
	d.mu.Lock()
	for _, l := range d.leases {
		l.cancel()
	}
	d.mu.Unlock()
	return nil
}

func (d *Daemon) takePIDFile() error {
	pf := d.cfg.Dirs.PIDFile()
	if b, err := os.ReadFile(pf); err == nil {
		if pid, _ := strconv.Atoi(strings.TrimSpace(string(b))); pid != os.Getpid() && chrome.Alive(pid) {
			return fmt.Errorf("clankerd already running for vm %q (pid %d)", d.cfg.VM, pid)
		}
	}
	return os.WriteFile(pf, []byte(strconv.Itoa(os.Getpid())+"\n"), 0o600)
}

func (d *Daemon) listen() (net.Listener, error) {
	sock := d.cfg.Dirs.Socket()
	if c, err := net.DialTimeout("unix", sock, time.Second); err == nil {
		c.Close()
		return nil, fmt.Errorf("clankerd already listening on %s", sock)
	}
	os.Remove(sock)
	ln, err := net.Listen("unix", sock)
	if err != nil {
		return nil, err
	}
	if err := os.Chmod(sock, 0o600); err != nil {
		ln.Close()
		return nil, err
	}
	return ln, nil
}

func (d *Daemon) restore() error {
	st, err := state.Load(d.cfg.Dirs.StateFile())
	if err != nil {
		return fmt.Errorf("loading state: %w", err)
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	for _, sl := range st.Leases {
		if sl.Slot < 0 || sl.Slot >= d.cfg.Slots || wire.ValidateName(sl.Name) != nil {
			d.log.Warn("dropping lease that no longer fits the configuration", "name", sl.Name, "slot", sl.Slot)
			continue
		}
		if !chrome.Alive(sl.ChromePID) {
			sl.ChromePID = 0
		}
		l := &lease{Lease: sl}
		d.leases[l.Name] = l
		warns, err := d.wire(l)
		for _, w := range warns {
			d.log.Warn(w, "name", l.Name)
		}
		if err != nil {
			d.log.Warn("restoring lease failed", "name", l.Name, "err", err)
		}
		d.log.Info("restored lease", "name", l.Name, "slot", l.Slot, "chrome_pid", l.ChromePID)
	}
	d.refreshNames()
	return nil
}

func (d *Daemon) refreshNames() {
	names := map[string]bool{}
	for _, l := range d.leases {
		for _, h := range l.Hosts {
			names[h] = true
		}
	}
	d.namesMu.Lock()
	d.names = names
	d.namesMu.Unlock()
}

func (d *Daemon) save() error {
	var st state.State
	for _, l := range d.sorted() {
		st.Leases = append(st.Leases, l.Lease)
	}
	return state.Save(d.cfg.Dirs.StateFile(), &st)
}

func (d *Daemon) sorted() []*lease {
	out := make([]*lease, 0, len(d.leases))
	for _, l := range d.leases {
		out = append(out, l)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Slot < out[j].Slot })
	return out
}

// wire (re)creates the pieces of a lease that live in this process or in the VM, and returns warnings.
// It is safe to call repeatedly.
func (d *Daemon) wire(l *lease) ([]string, error) {
	var warns []string
	if l.cancel == nil {
		ctx, cancel := context.WithCancel(d.ctx)
		cdp, err := net.Listen("tcp", net.JoinHostPort(d.cfg.RelayBind, strconv.Itoa(d.cdpPort(l.Slot))))
		if err != nil {
			cancel()
			return nil, fmt.Errorf("cdp relay: %w", err)
		}
		go relay.Serve(ctx, cdp, net.JoinHostPort("127.0.0.1", strconv.Itoa(d.chromePort(l.Slot))))
		l.ctx, l.cancel, l.forwarders = ctx, cancel, map[netip.Addr]context.CancelFunc{}
	}
	warns = append(warns, d.syncForwarders(l)...)
	if err := d.ensureVMRelay(l); err != nil {
		return warns, err
	}
	return warns, nil
}

// syncForwarders makes the app port reachable on every announced non-loopback address.
// smolvm publishes the port on loopback only.
func (d *Daemon) syncForwarders(l *lease) []string {
	var warns []string
	if len(d.cfg.MDNSSubnets) == 0 {
		warns = append(warns, "no mDNS subnets configured (CLANKERD_MDNS_SUBNETS); nothing announced")
	}
	want := map[netip.Addr]bool{}
	announced := announceAddrs(d.cfg.MDNSSubnets)
	for _, a := range announced {
		if !a.IsLoopback() {
			want[a] = true
		}
	}
	if len(d.cfg.MDNSSubnets) > 0 && len(announced) == 0 {
		warns = append(warns, "no local address inside the mDNS subnets; nothing announced")
	}
	for a, stop := range l.forwarders {
		if !want[a] {
			stop()
			delete(l.forwarders, a)
		}
	}
	port := strconv.Itoa(d.appPort(l.Slot))
	for a := range want {
		if _, ok := l.forwarders[a]; ok {
			continue
		}
		ln, err := net.Listen("tcp", net.JoinHostPort(a.String(), port))
		if err != nil {
			warns = append(warns, fmt.Sprintf("cannot forward %s: %v", net.JoinHostPort(a.String(), port), err))
			continue
		}
		ctx, cancel := context.WithCancel(l.ctx)
		l.forwarders[a] = cancel
		go relay.Serve(ctx, ln, net.JoinHostPort("127.0.0.1", port))
	}
	return warns
}

const startRelayScript = `pf=$1
if [ -f "$pf" ] && kill -0 "$(cat "$pf")" 2>/dev/null; then exit 0; fi
mkdir -p "$(dirname "$pf")" || exit 1
setsid -f clankerctl relay --pidfile "$pf" --listen "$2" --target "$3" </dev/null >/dev/null 2>&1
`

const stopRelayScript = `pf=$1
if [ -f "$pf" ]; then kill "$(cat "$pf")" 2>/dev/null; fi
rm -f "$pf"
exit 0
`

func (d *Daemon) vmRunning() error {
	st, err := d.vm.Status(d.ctx)
	if err != nil {
		return err
	}
	if st != backend.Running {
		return fmt.Errorf("vm %q is %s, not running; start it with `clankerctl smol up`", d.cfg.VM, st)
	}
	return nil
}

// ensureVMRelay starts the VM-side relay unless its pidfile names a live process.
// The relay command is started detached: smolvm kills everything still attached to an exec.
func (d *Daemon) ensureVMRelay(l *lease) error {
	if err := d.vmRunning(); err != nil {
		return err
	}
	host := d.cfg.HostAddr
	if host == "" {
		host = "gateway"
	}
	port := strconv.Itoa(d.cdpPort(l.Slot))
	ctx, cancel := context.WithTimeout(d.ctx, execTimeout)
	defer cancel()
	if out, err := d.vm.Exec(ctx, "sh", "-c", startRelayScript, "sh",
		d.pidFile(l.Name), net.JoinHostPort("127.0.0.1", port), net.JoinHostPort(host, port)); err != nil {
		return fmt.Errorf("starting the relay in the VM: %w", err)
	} else if strings.TrimSpace(out) != "" {
		d.log.Debug("relay start output", "out", out)
	}
	return nil
}

func (d *Daemon) stopVMRelay(l *lease) {
	if d.vmRunning() != nil {
		return
	}
	ctx, cancel := context.WithTimeout(d.ctx, execTimeout)
	defer cancel()
	if _, err := d.vm.Exec(ctx, "sh", "-c", stopRelayScript, "sh", d.pidFile(l.Name)); err != nil {
		d.log.Warn("stopping the relay in the VM failed", "name", l.Name, "err", err)
	}
}

func (d *Daemon) view(l *lease) wire.Lease {
	return wire.Lease{
		Name: l.Name, Slot: l.Slot,
		AppPort: d.appPort(l.Slot), CDPPort: d.cdpPort(l.Slot), ChromePort: d.chromePort(l.Slot),
		Hosts:         append([]string(nil), l.Hosts...),
		CDPURL:        fmt.Sprintf("http://localhost:%d", d.cdpPort(l.Slot)),
		ChromeRunning: chrome.Alive(l.ChromePID),
	}
}
