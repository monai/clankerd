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

	"github.com/monai/clankers/sandbox/clankerd/internal/backend"
	"github.com/monai/clankers/sandbox/clankerd/internal/chrome"
	"github.com/monai/clankers/sandbox/clankerd/internal/config"
	"github.com/monai/clankers/sandbox/clankerd/internal/mdns"
	"github.com/monai/clankers/sandbox/clankerd/internal/relay"
	"github.com/monai/clankers/sandbox/clankerd/internal/state"
	"github.com/monai/clankers/sandbox/clankerd/internal/wire"
)

const execTimeout = 30 * time.Second

// lease has two locks. op serialises operations on the lease and is held across slow I/O (VM exec,
// Chrome shutdown). mu guards the few fields readers need and is only ever held briefly, so listing
// leases never waits on a slow operation.
type lease struct {
	op         sync.Mutex
	ctx        context.Context
	cancel     context.CancelFunc
	forwarders map[netip.Addr]context.CancelFunc
	dead       bool

	mu    sync.Mutex
	state state.Lease // Name and Slot never change; Hosts and ChromePID are guarded by mu
	ready bool        // acquired successfully; a lease still being acquired is invisible to readers
}

// stopLocal closes the lease's relay and forwarders in this process. The caller holds l.op.
func (l *lease) stopLocal() {
	if l.cancel == nil {
		return
	}
	l.cancel()
	for _, stop := range l.forwarders {
		stop()
	}
}

func (l *lease) name() string { return l.state.Name }
func (l *lease) slot() int    { return l.state.Slot }

func (l *lease) snapshot() (state.Lease, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	s := l.state
	s.Hosts = append([]string(nil), s.Hosts...)
	return s, l.ready
}

func (l *lease) update(f func(*state.Lease)) {
	l.mu.Lock()
	defer l.mu.Unlock()
	f(&l.state)
}

// Daemon's locks, outermost first: lease.op, saveMu, mu, lease.mu. mu is never held across I/O.
type Daemon struct {
	cfg *config.Config
	vm  backend.Backend
	log *slog.Logger
	ctx context.Context

	mu     sync.Mutex
	leases map[string]*lease
	claims map[string]string // hostname -> lease name

	saveMu  sync.Mutex
	namesMu sync.RWMutex
	names   map[string]bool
}

func New(ctx context.Context, cfg *config.Config, vm backend.Backend, log *slog.Logger) *Daemon {
	return &Daemon{ctx: ctx, cfg: cfg, vm: vm, log: log, leases: map[string]*lease{}, claims: map[string]string{}, names: map[string]bool{}}
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
		if l.cancel != nil {
			l.cancel()
		}
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

func (d *Daemon) profile(name string) string { return d.cfg.Dirs.Profile(name) }

func (d *Daemon) restore() error {
	st, err := state.Load(d.cfg.Dirs.StateFile())
	if err != nil {
		return fmt.Errorf("loading state: %w", err)
	}
	for _, sl := range st.Leases {
		if sl.Slot < 0 || sl.Slot >= d.cfg.Slots || wire.ValidateName(sl.Name) != nil {
			d.log.Warn("dropping lease that no longer fits the configuration", "name", sl.Name, "slot", sl.Slot)
			continue
		}
		if !chrome.Running(sl.ChromePID, d.profile(sl.Name)) {
			sl.ChromePID = 0
		}
		l := &lease{state: sl, ready: true}
		d.leases[l.name()] = l
		for _, h := range sl.Hosts {
			d.claims[h] = sl.Name
		}
		warns, err := d.ensureLease(l)
		for _, w := range warns {
			d.log.Warn(w, "name", l.name())
		}
		if err != nil {
			d.log.Warn("restoring lease failed", "name", l.name(), "err", err)
		}
		d.log.Info("restored lease", "name", l.name(), "slot", l.slot(), "chrome_pid", sl.ChromePID)
	}
	d.refreshNames()
	return nil
}

func (d *Daemon) readyLeases() []*lease {
	d.mu.Lock()
	all := make([]*lease, 0, len(d.leases))
	for _, l := range d.leases {
		all = append(all, l)
	}
	d.mu.Unlock()
	out := all[:0]
	for _, l := range all {
		if _, ready := l.snapshot(); ready {
			out = append(out, l)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].slot() < out[j].slot() })
	return out
}

func (d *Daemon) refreshNames() {
	names := map[string]bool{}
	for _, l := range d.readyLeases() {
		s, _ := l.snapshot()
		for _, h := range s.Hosts {
			names[h] = true
		}
	}
	d.namesMu.Lock()
	d.names = names
	d.namesMu.Unlock()
}

func (d *Daemon) save() error {
	d.saveMu.Lock()
	defer d.saveMu.Unlock()
	var st state.State
	for _, l := range d.readyLeases() {
		s, _ := l.snapshot()
		st.Leases = append(st.Leases, s)
	}
	return state.Save(d.cfg.Dirs.StateFile(), &st)
}

// ensureLease (re)creates the pieces of a lease that live in this process or in the VM, and returns
// warnings. The caller holds l.op. It is safe to call repeatedly.
func (d *Daemon) ensureLease(l *lease) ([]string, error) {
	var warns []string
	if l.cancel == nil {
		ctx, cancel := context.WithCancel(d.ctx)
		cdps, errs, err := relay.ListenAll(d.cfg.RelayBind, strconv.Itoa(d.cdpPort(l.slot())))
		if err != nil {
			cancel()
			return nil, fmt.Errorf("cdp relay: %w", err)
		}
		for _, e := range errs {
			d.log.Warn("cdp relay: address skipped", "name", l.name(), "err", e)
		}
		for _, cdp := range cdps {
			go relay.Serve(ctx, cdp, relay.Target(net.JoinHostPort("localhost", strconv.Itoa(d.chromePort(l.slot())))))
		}
		l.ctx, l.forwarders = ctx, map[netip.Addr]context.CancelFunc{}
		l.cancel = func() { // close synchronously so the ports are free when this returns
			cancel()
			for _, cdp := range cdps {
				cdp.Close()
			}
		}
	}
	warns = append(warns, d.syncForwarders(l)...)
	if err := d.ensureVMRelay(l); err != nil {
		return warns, err
	}
	return warns, nil
}

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
	port := strconv.Itoa(d.appPort(l.slot()))
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
		l.forwarders[a] = func() { cancel(); ln.Close() }
		go relay.Serve(ctx, ln, relay.Target(net.JoinHostPort("localhost", port)))
	}
	return warns
}

const startRelayScript = `pf=$1
if [ -f "$pf" ] && kill -0 "$(cat "$pf")" 2>/dev/null; then exit 0; fi
mkdir -p "$(dirname "$pf")" || exit 1
setsid -f clankerctl relay --pidfile "$pf" --listen "$2" --listen "$3" --target "$4" </dev/null >/dev/null 2>&1
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
func (d *Daemon) ensureVMRelay(l *lease) error {
	if err := d.vmRunning(); err != nil {
		return err
	}
	host := d.cfg.HostAddr
	if host == "" {
		host = "gateway"
	}
	port := strconv.Itoa(d.cdpPort(l.slot()))
	ctx, cancel := context.WithTimeout(d.ctx, execTimeout)
	defer cancel()
	if out, err := d.vm.Exec(ctx, "sh", "-c", startRelayScript, "sh",
		d.pidFile(l.name()), net.JoinHostPort("127.0.0.1", port), net.JoinHostPort("::1", port), net.JoinHostPort(host, port)); err != nil {
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
	if _, err := d.vm.Exec(ctx, "sh", "-c", stopRelayScript, "sh", d.pidFile(l.name())); err != nil {
		d.log.Warn("stopping the relay in the VM failed", "name", l.name(), "err", err)
	}
}

func (d *Daemon) view(l *lease) wire.Lease {
	s, _ := l.snapshot()
	return wire.Lease{
		Name: s.Name, Slot: s.Slot,
		AppPort: d.appPort(s.Slot), CDPPort: d.cdpPort(s.Slot), ChromePort: d.chromePort(s.Slot),
		Hosts:         s.Hosts,
		CDPURL:        fmt.Sprintf("http://localhost:%d", d.cdpPort(s.Slot)),
		ChromeRunning: chrome.Running(s.ChromePID, d.profile(s.Name)),
	}
}
