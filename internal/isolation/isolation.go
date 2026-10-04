// Package isolation says which side of the sandbox boundary a process runs on.
package isolation

import (
	"os"
	"strings"
)

type Kind int

const (
	Host Kind = iota
	VM
	Docker
)

func (k Kind) String() string { return [...]string{"host", "vm", "docker"}[k] }

// Sandboxed reports whether the process runs inside a VM or container rather than on the host.
func (k Kind) Sandboxed() bool { return k != Host }

// Probes are the files Detect reads. Tests point them elsewhere through CLANKERD_CMDLINE_PATH and
// CLANKERD_DOCKERENV_PATH, because the real ones cannot be faked without root.
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

// Detect reads the platform's own markers. A smolvm guest's kernel command line carries
// SMOLVM_MACHINE_NAME; a Docker container has /.dockerenv. The VM check comes first because
// a container running inside a VM shares the VM's kernel command line.
func Detect() Kind { return DefaultProbes().Detect() }

func (p Probes) Detect() Kind {
	if b, err := os.ReadFile(p.Cmdline); err == nil && strings.Contains(string(b), "SMOLVM_MACHINE_NAME=") {
		return VM
	}
	if _, err := os.Stat(p.DockerEnv); err == nil {
		return Docker
	}
	return Host
}
