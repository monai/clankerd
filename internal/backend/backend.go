// Package backend drives the VM. Smolvm is the only implementation today; the interface leaves room
// for a Docker backend.
package backend

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"
)

type State int

const (
	Missing State = iota
	Stopped
	Running
)

func (s State) String() string { return [...]string{"missing", "stopped", "running"}[s] }

// CreateSpec is everything `smol up` decides about the VM.
type CreateSpec struct {
	Image      string
	CPUs       int
	Mem        int
	Storage    int
	Net        bool
	NetBackend string
	User       string
	Volumes    []string
	Env        []string
	Init       []string
	PortFrom   int
	PortTo     int
	Socket     string
	GuestSock  string
}

// Fingerprint identifies the configuration a VM was created from.
func (c CreateSpec) Fingerprint() string {
	b, _ := json.Marshal(c)
	return fmt.Sprintf("%x", sha256.Sum256(b))
}

type Backend interface {
	Create(ctx context.Context, spec CreateSpec) error
	Start(ctx context.Context) error
	// Exec runs argv inside the VM and returns its combined output.
	Exec(ctx context.Context, argv ...string) (string, error)
	Stop(ctx context.Context) error
	Delete(ctx context.Context) error
	Status(ctx context.Context) (State, error)
}

// Smolvm runs the `smolvm` command line.
type Smolvm struct {
	Name string
	Bin  string
}

func NewSmolvm(name string) *Smolvm { return &Smolvm{Name: name, Bin: "smolvm"} }

func (s *Smolvm) run(ctx context.Context, args ...string) (string, error) {
	out, err := exec.CommandContext(ctx, s.Bin, args...).CombinedOutput()
	if err != nil {
		return string(out), fmt.Errorf("smolvm %s: %w: %s", strings.Join(args[:min(2, len(args))], " "), err, strings.TrimSpace(string(out)))
	}
	return string(out), nil
}

func (s *Smolvm) Create(ctx context.Context, c CreateSpec) error {
	args := []string{"machine", "create", "--name", s.Name}
	opt := func(flag, v string) {
		if v != "" {
			args = append(args, flag, v)
		}
	}
	optInt := func(flag string, n int) {
		if n > 0 {
			opt(flag, fmt.Sprint(n))
		}
	}
	opt("--image", c.Image)
	optInt("--cpus", c.CPUs)
	optInt("--mem", c.Mem)
	optInt("--storage", c.Storage)
	if c.Net {
		args = append(args, "--net")
	}
	opt("--net-backend", c.NetBackend)
	opt("--user", c.User)
	for _, v := range c.Volumes {
		args = append(args, "--volume", v)
	}
	for _, e := range c.Env {
		args = append(args, "--env", e)
	}
	for _, i := range c.Init {
		args = append(args, "--init", i)
	}
	args = append(args, "-p", fmt.Sprintf("%d-%d:%d-%d", c.PortFrom, c.PortTo, c.PortFrom, c.PortTo))
	if c.Socket != "" {
		args = append(args, "--mount-socket", c.Socket+":"+c.GuestSock)
	}
	_, err := s.run(ctx, args...)
	return err
}

func (s *Smolvm) Start(ctx context.Context) error {
	_, err := s.run(ctx, "machine", "start", "--name", s.Name)
	return err
}

func (s *Smolvm) Exec(ctx context.Context, argv ...string) (string, error) {
	return s.run(ctx, append([]string{"machine", "exec", "--name", s.Name, "--"}, argv...)...)
}

func (s *Smolvm) Stop(ctx context.Context) error {
	_, err := s.run(ctx, "machine", "stop", "--name", s.Name)
	return err
}

func (s *Smolvm) Delete(ctx context.Context) error {
	_, err := s.run(ctx, "machine", "delete", "--name", s.Name, "--force")
	return err
}

// Status treats a failing `machine status` as "no such machine".
func (s *Smolvm) Status(ctx context.Context) (State, error) {
	if _, err := exec.LookPath(s.Bin); err != nil {
		return Missing, fmt.Errorf("%s not found on PATH", s.Bin)
	}
	out, err := exec.CommandContext(ctx, s.Bin, "machine", "status", "--name", s.Name).CombinedOutput()
	if err != nil {
		return Missing, nil
	}
	lower := strings.ToLower(string(out))
	if strings.Contains(lower, "running") && !strings.Contains(lower, "not running") {
		return Running, nil
	}
	return Stopped, nil
}
