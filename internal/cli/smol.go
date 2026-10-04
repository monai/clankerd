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
	"time"

	"github.com/monai/clankers/sandbox/clankerd/internal/backend"
	"github.com/monai/clankers/sandbox/clankerd/internal/config"
	"github.com/monai/clankers/sandbox/clankerd/internal/wire"
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
	if t.vm {
		return errors.New("smol commands run on the host, not in the VM")
	}
	vm := backend.NewSmolvm(t.cfg.VM)
	ctx := context.Background()
	switch sub {
	case "up":
		return c.smolUp(ctx, t, vm)
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
	if !c.daemonUp(t) {
		if err := c.startDaemon(t); err != nil {
			return err
		}
	}
	st, err := vm.Status(ctx)
	if err != nil {
		return err
	}
	if st == backend.Missing {
		env := append([]string{}, cfg.Smol.Env...)
		for k, v := range map[string]string{"HOST_UID": strconv.Itoa(os.Getuid()), "HOST_GID": strconv.Itoa(os.Getgid())} {
			if !hasEnv(env, k) {
				env = append(env, k+"="+v)
			}
		}
		if err := vm.Create(ctx, backend.CreateSpec{
			Image: cfg.Smol.Image, CPUs: cfg.Smol.CPUs, Mem: cfg.Smol.Mem, Storage: cfg.Smol.Storage,
			Net: cfg.Smol.Net, NetBackend: cfg.Smol.NetBackend, User: cfg.Smol.User,
			Volumes: cfg.Smol.Volumes, Env: env, Init: cfg.Smol.Init,
			PortFrom: cfg.AppPortBase, PortTo: cfg.AppPortBase + cfg.Slots - 1,
			Socket: cfg.Dirs.Socket(), GuestSock: wire.GuestSocket,
		}); err != nil {
			return err
		}
	}
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
	fmt.Fprintf(c.stdout, "up: vm=%s apps=%d-%d ctl=%s\n", cfg.VM, cfg.AppPortBase, cfg.AppPortBase+cfg.Slots-1, t.sock)
	return nil
}

func hasEnv(env []string, k string) bool {
	for _, e := range env {
		if strings.HasPrefix(e, k+"=") {
			return true
		}
	}
	return false
}

func (c *ctl) smolDown(ctx context.Context, t *target, vm backend.Backend) error {
	if resp, err := wire.Call(t.sock, wire.Request{Op: wire.OpLeaseList}); err == nil {
		for _, l := range resp.Leases {
			if _, err := wire.Call(t.sock, wire.Request{Op: wire.OpLeaseRelease, Name: l.Name}); err != nil {
				fmt.Fprintf(c.stderr, "warning: releasing %s: %v\n", l.Name, err)
			}
		}
	}
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
