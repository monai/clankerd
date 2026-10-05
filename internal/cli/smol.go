package cli

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"text/template"
	"time"

	"github.com/monai/clankers/clankerd/internal/backend"
	"github.com/monai/clankers/clankerd/internal/config"
	"github.com/monai/clankers/clankerd/internal/lock"
	"github.com/monai/clankers/clankerd/internal/wire"
)

func (c *ctl) smol(args []string) error {
	if len(args) == 0 {
		return usageError{}
	}
	sub := args[0]
	fs := flag.NewFlagSet("smol "+sub, flag.ContinueOnError)
	cf := config.AddFlags(fs)
	pos, err := parseArgs(fs, args[1:])
	if err != nil {
		return err
	}
	if len(pos) > 0 {
		return usageError{"unexpected arguments: " + strings.Join(pos, " ")}
	}
	t, err := connect(cf)
	if err != nil {
		return err
	}
	if t.kind.Sandboxed() {
		return fmt.Errorf("smol commands run on the host, not in a %s", t.kind)
	}
	vm := backend.NewSmolvm(t.cfg.VM)
	ctx := context.Background()
	if sub != "status" {
		if err := os.MkdirAll(t.cfg.Dirs.Runtime, 0o700); err != nil {
			return err
		}
		l, err := lock.Acquire(t.cfg.Dirs.LockFile())
		if err != nil {
			return err
		}
		defer l.Release()
	}
	switch sub {
	case "up":
		return c.smolUp(ctx, t, vm)
	case "start":
		return c.smolStart(ctx, t, vm)
	case "stop":
		return c.smolStop(ctx, t, vm)
	case "down":
		return c.smolDown(ctx, t, vm)
	case "status":
		return c.smolStatus(ctx, t, vm)
	}
	return usageError{"unknown command smol " + sub}
}

func (c *ctl) daemonUp(t *target) bool {
	_, err := wire.Call(t.sock, wire.Request{Op: wire.OpLeaseList})
	return err == nil
}

func daemonBinary() (string, error) {
	if exe, err := os.Executable(); err == nil {
		if p := filepath.Join(filepath.Dir(exe), "clankerd"); fileExists(p) {
			return p, nil
		}
	}
	return exec.LookPath("clankerd")
}

func fileExists(p string) bool { _, err := os.Stat(p); return err == nil }

func (c *ctl) startDaemon(t *target) error {
	bin, err := daemonBinary()
	if err != nil {
		return errors.New("clankerd not found next to clankerctl or on PATH")
	}
	cmd := exec.Command(bin, append([]string{"run"}, t.cf.Args()...)...)
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	if devnull, err := os.OpenFile(os.DevNull, os.O_RDWR, 0); err == nil {
		defer devnull.Close()
		cmd.Stdin, cmd.Stdout, cmd.Stderr = devnull, devnull, devnull
	}
	if err := cmd.Start(); err != nil {
		return err
	}
	exited := make(chan error, 1)
	go func() { exited <- cmd.Wait() }()
	for i := 0; i < 100; i++ {
		select {
		case <-exited:
			return fmt.Errorf("clankerd exited at startup; see %s", t.cfg.Dirs.LogFile())
		case <-time.After(100 * time.Millisecond):
		}
		if c.daemonUp(t) {
			return nil
		}
	}
	return fmt.Errorf("clankerd did not start; see %s", t.cfg.Dirs.LogFile())
}

func (c *ctl) smolUp(ctx context.Context, t *target, vm backend.Backend) error {
	cfg := t.cfg
	vals := templateData{UID: os.Getuid(), GID: os.Getgid()}
	volumes, err := vals.render(cfg.Smol.Volumes)
	if err != nil {
		return fmt.Errorf("smol.volumes: %w", err)
	}
	env, err := vals.render(cfg.Smol.Env)
	if err != nil {
		return fmt.Errorf("smol.env: %w", err)
	}
	init, err := vals.render(cfg.Smol.Init)
	if err != nil {
		return fmt.Errorf("smol.init: %w", err)
	}
	spec := backend.CreateSpec{
		Image: cfg.Smol.Image, CPUs: cfg.Smol.CPUs, Mem: cfg.Smol.Mem, Storage: cfg.Smol.Storage,
		Net: cfg.Smol.Net, NetBackend: cfg.Smol.NetBackend, User: cfg.Smol.User,
		Volumes: volumes, Env: env, Init: init, Cmd: cfg.Smol.Cmd,
		PortFrom: cfg.AppPortBase, PortTo: cfg.AppPortBase + cfg.Slots - 1,
		Socket: cfg.Dirs.Socket(), GuestSock: wire.GuestSocket,
	}
	if !c.daemonUp(t) {
		if err := c.startDaemon(t); err != nil {
			return err
		}
	}
	st, err := vm.Status(ctx)
	if err != nil {
		return err
	}
	fpFile := filepath.Join(cfg.Dirs.State, "vm-spec")
	if st == backend.Missing {
		if err := vm.Create(ctx, spec); err != nil {
			return err
		}
		if err := os.WriteFile(fpFile, []byte(spec.Fingerprint()), 0o600); err != nil {
			return err
		}
	} else if b, err := os.ReadFile(fpFile); err != nil || string(b) != spec.Fingerprint() {
		return fmt.Errorf("vm %q exists but was not created from the current configuration "+
			"(config changed, or the VM came from elsewhere); recreate it with `clankerctl smol down` then `smol up`", cfg.VM)
	}
	if err := c.startVM(ctx, t, vm, st); err != nil {
		return err
	}
	fmt.Fprintf(c.stdout, "up: vm=%s ports=%d-%d ctl=%s\n", cfg.VM, cfg.AppPortBase, cfg.AppPortBase+cfg.Slots-1, t.sock)
	return nil
}

func (c *ctl) startVM(ctx context.Context, t *target, vm backend.Backend, st backend.State) error {
	if st != backend.Running {
		if err := vm.Start(ctx); err != nil {
			return err
		}
	}
	resp, err := wire.Call(t.sock, wire.Request{Op: wire.OpResync})
	if err != nil {
		return err
	}
	c.warn(resp)
	return nil
}

func (c *ctl) smolStart(ctx context.Context, t *target, vm backend.Backend) error {
	st, err := vm.Status(ctx)
	if err != nil {
		return err
	}
	if st == backend.Missing {
		return fmt.Errorf("vm %q does not exist; create it with `clankerctl smol up`", t.cfg.VM)
	}
	if !c.daemonUp(t) {
		if err := c.startDaemon(t); err != nil {
			return err
		}
	}
	if err := c.startVM(ctx, t, vm, st); err != nil {
		return err
	}
	fmt.Fprintln(c.stdout, "start: vm="+t.cfg.VM)
	return nil
}

func (c *ctl) releaseLeases(t *target) {
	resp, err := wire.Call(t.sock, wire.Request{Op: wire.OpLeaseList})
	if err != nil {
		return
	}
	for _, l := range resp.Leases {
		if _, err := wire.Call(t.sock, wire.Request{Op: wire.OpLeaseRelease, Name: l.Name}); err != nil {
			fmt.Fprintf(c.stderr, "warning: releasing %s: %v\n", l.Name, err)
		}
	}
}

func (c *ctl) smolStop(ctx context.Context, t *target, vm backend.Backend) error {
	c.releaseLeases(t)
	c.stopDaemon(t)
	st, err := vm.Status(ctx)
	if err != nil {
		return err
	}
	if st == backend.Running {
		if err := vm.Stop(ctx); err != nil {
			return err
		}
	}
	fmt.Fprintln(c.stdout, "stop: vm="+t.cfg.VM)
	return nil
}

func (c *ctl) smolDown(ctx context.Context, t *target, vm backend.Backend) error {
	c.releaseLeases(t)
	c.stopDaemon(t)
	st, err := vm.Status(ctx)
	if err != nil {
		return err
	}
	if st == backend.Running {
		if err := vm.Stop(ctx); err != nil {
			fmt.Fprintln(c.stderr, "warning:", err)
		}
	}
	if st != backend.Missing {
		if err := vm.Delete(ctx); err != nil {
			return err
		}
	}
	os.Remove(filepath.Join(t.cfg.Dirs.State, "vm-spec"))
	fmt.Fprintln(c.stdout, "down: vm="+t.cfg.VM)
	return nil
}

func (c *ctl) stopDaemon(t *target) {
	b, err := os.ReadFile(t.cfg.Dirs.PIDFile())
	if err != nil {
		return
	}
	pid, _ := strconv.Atoi(strings.TrimSpace(string(b)))
	if pid <= 0 || syscall.Kill(pid, 0) != nil {
		return
	}
	syscall.Kill(pid, syscall.SIGTERM)
	for i := 0; i < 50; i++ {
		if syscall.Kill(pid, 0) != nil {
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	syscall.Kill(pid, syscall.SIGKILL)
}

func (c *ctl) smolStatus(ctx context.Context, t *target, vm backend.Backend) error {
	if resp, err := wire.Call(t.sock, wire.Request{Op: wire.OpLeaseList}); err != nil {
		fmt.Fprintln(c.stdout, "daemon: stopped")
	} else {
		pid := "?"
		if b, err := os.ReadFile(t.cfg.Dirs.PIDFile()); err == nil {
			pid = strings.TrimSpace(string(b))
		}
		fmt.Fprintf(c.stdout, "daemon: running (pid %s, %d leases)\n", pid, len(resp.Leases))
	}
	st, err := vm.Status(ctx)
	if err != nil {
		return err
	}
	fmt.Fprintln(c.stdout, "vm: "+st.String())
	return nil
}

type templateData struct{ UID, GID int }

func (d templateData) render(in []string) ([]string, error) {
	out := make([]string, len(in))
	for i, v := range in {
		t, err := template.New("").Option("missingkey=error").Parse(v)
		if err != nil {
			return nil, err
		}
		var b strings.Builder
		if err := t.Execute(&b, d); err != nil {
			return nil, err
		}
		out[i] = b.String()
	}
	return out, nil
}
