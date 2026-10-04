// Package isolation says which side of the sandbox boundary a process runs on.
package isolation

import (
	"os"
	"strings"
)

type Kind int

const (
	Host Kind = iota
	Smolmachine
	Docker
)

func (k Kind) String() string { return [...]string{"host", "smolmachine", "docker"}[k] }

// Sandboxed reports whether the process runs inside a smolmachine or container rather than on the host.
func (k Kind) Sandboxed() bool { return k != Host }

type Probes struct {
	Cmdline   string
	DockerEnv string
}

func DefaultProbes() Probes {
	p := Probes{Cmdline: "/proc/cmdline", DockerEnv: "/.dockerenv"}
	if v, ok := os.LookupEnv("CLANKERD_CMDLINE_PATH"); ok {
		p.Cmdline = v
	}
	if v, ok := os.LookupEnv("CLANKERD_DOCKERENV_PATH"); ok {
		p.DockerEnv = v
	}
	return p
}

func Detect() Kind { return DefaultProbes().Detect() }

func (p Probes) Detect() Kind {
	if b, err := os.ReadFile(p.Cmdline); err == nil && strings.Contains(string(b), "SMOLVM_MACHINE_NAME=") {
		return Smolmachine
	}
	if _, err := os.Stat(p.DockerEnv); err == nil {
		return Docker
	}
	return Host
}
