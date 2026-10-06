package cli

import (
	"flag"
	"fmt"
	"io"
	"os"

	"github.com/monai/clankers/clankerd/internal/wire"
)

const guestUsage = `usage: guestctl <command>

  lease acquire NAME [HOST.local ...]   reserve ports and .local names (repeat to change the hostnames)
  lease show NAME                 see a lease (--json for structured output)
  lease release NAME [--purge]    free a lease; --purge also deletes its Chrome profile
  browser start|stop NAME         start or stop the host Chrome wired to the lease
  version
`

// Guest runs guestctl and returns its exit code.
func Guest(args []string, stdout, stderr io.Writer) int {
	c := &ctl{"guestctl", guestUsage, stdout, stderr}
	return c.main(func() error { return c.runGuest(args) })
}

func guestSocket() (string, error) {
	sock := os.Getenv("CLANKERD_GUEST_SOCKET")
	if sock == "" {
		sock = wire.GuestSocket
	}
	if fi, err := os.Stat(sock); err != nil || fi.Mode()&os.ModeSocket == 0 {
		return "", fmt.Errorf("the control socket %s is missing; recreate it from the host with `hostctl smol down`, `smol up`", sock)
	}
	return sock, nil
}

func (c *ctl) runGuest(args []string) error {
	if len(args) == 0 {
		return usageError{}
	}
	cmd, rest := args[0], args[1:]
	switch cmd {
	case "version":
		c.version()
		return nil
	case "relay":
		return runRelay(rest)
	case "lease", "browser":
		if len(rest) == 0 {
			return usageError{}
		}
		fs := flag.NewFlagSet(cmd+" "+rest[0], flag.ContinueOnError)
		return c.leaseCmd(fs, cmd, rest[0], rest[1:],
			[]string{"lease acquire", "lease show", "lease release", "browser start", "browser stop"},
			guestSocket, "run `hostctl smol up` on the host")
	}
	return usageError{"unknown command " + cmd}
}
