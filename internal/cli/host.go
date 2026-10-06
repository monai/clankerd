package cli

import (
	"flag"
	"io"
	"os"

	"github.com/monai/clankers/clankerd/internal/config"
)

const hostUsage = `usage: hostctl <command>

  smol up|down|status             create+start / delete the VM and the daemon
  smol start|stop                 start (applying config changes) / stop an existing VM and the daemon, keeping the VM
  lease list | lease show NAME    see leases (--json for structured output)
  lease release NAME [--purge]    free a lease; --purge also deletes its Chrome profile
  version

Every command also accepts --home, --config and one flag per setting (--vm, --slots, --mdns-subnets ...); each has a CLANKERD_* environment variable.
`

// Host runs hostctl and returns its exit code.
func Host(args []string, stdout, stderr io.Writer) int {
	c := &ctl{"hostctl", hostUsage, stdout, stderr}
	return c.main(func() error { return c.runHost(args) })
}

type target struct {
	sock string
	cfg  *config.Config
	cf   *config.Flags
}

func connect(cf *config.Flags) (*target, error) {
	cwd, err := os.Getwd()
	if err != nil {
		return nil, err
	}
	cfg, err := config.Load(cf, cwd)
	if err != nil {
		return nil, err
	}
	return &target{sock: cfg.Dirs.Socket(), cfg: cfg, cf: cf}, nil
}

func (c *ctl) runHost(args []string) error {
	if len(args) == 0 {
		return usageError{}
	}
	cmd, rest := args[0], args[1:]
	switch cmd {
	case "version":
		c.version()
		return nil
	case "smol":
		return c.smol(rest)
	case "lease":
		if len(rest) == 0 {
			return usageError{}
		}
		fs := flag.NewFlagSet("lease "+rest[0], flag.ContinueOnError)
		cf := config.AddFlags(fs)
		sock := func() (string, error) {
			t, err := connect(cf)
			if err != nil {
				return "", err
			}
			return t.sock, nil
		}
		return c.leaseCmd(fs, cmd, rest[0], rest[1:],
			[]string{"lease list", "lease show", "lease release"}, sock, "start it with `hostctl smol up`")
	}
	return usageError{"unknown command " + cmd}
}
