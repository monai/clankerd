// Package cli implements the clankerctl and clankerd command lines.
package cli

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"strings"
	"text/tabwriter"

	"github.com/monai/clankers/sandbox/clankerd/internal/config"
	"github.com/monai/clankers/sandbox/clankerd/internal/wire"
)

var Version = "dev"

const ctlUsage = `usage: clankerctl <command>

  smol up|down|status             create+start / delete the VM and the host daemon (host only)
  lease acquire NAME [HOST.local ...]   reserve ports and .local names (repeat to change the hostnames)
  lease release NAME [--purge]    free a lease; --purge also deletes its Chrome profile
  lease list | lease show NAME    see leases (--json for structured output)
  browser start|stop NAME         start or stop the host Chrome wired to the lease
  version

Every command also accepts --home, --config and one flag per setting (--vm, --slots, --mdns-subnets ...); each has a CLANKERD_* environment variable.
`

type ctl struct {
	stdout, stderr io.Writer
}

// Ctl runs clankerctl and returns its exit code.
func Ctl(args []string, stdout, stderr io.Writer) int {
	c := &ctl{stdout, stderr}
	err := c.run(args)
	if err == nil {
		return 0
	}
	var u usageError
	if errors.As(err, &u) {
		fmt.Fprint(stderr, ctlUsage)
		if u.msg != "" {
			fmt.Fprintln(stderr, "\n"+u.msg)
		}
		return 64
	}
	fmt.Fprintln(stderr, "clankerctl:", err)
	return 1
}

type usageError struct{ msg string }

func (u usageError) Error() string { return "usage: " + u.msg }

func (c *ctl) run(args []string) error {
	if len(args) == 0 {
		return usageError{}
	}
	cmd, rest := args[0], args[1:]
	switch cmd {
	case "version":
		fmt.Fprintf(c.stdout, "clankerctl %s (protocol %d)\n", Version, wire.Version)
		return nil
	case "relay":
		return runRelay(rest)
	case "smol":
		return c.smol(rest)
	case "lease", "browser":
		if len(rest) == 0 {
			return usageError{}
		}
		return c.leaseOrBrowser(cmd, rest[0], rest[1:])
	}
	return usageError{"unknown command " + cmd}
}

func parseArgs(fs *flag.FlagSet, args []string) ([]string, error) {
	fs.SetOutput(io.Discard)
	var pos []string
	for {
		if err := fs.Parse(args); err != nil {
			return nil, usageError{err.Error()}
		}
		args = fs.Args()
		if len(args) == 0 {
			return pos, nil
		}
		pos = append(pos, args[0])
		args = args[1:]
	}
}

type target struct {
	sock string
	vm   bool
	cfg  *config.Config
	cf   *config.Flags
}

func (t *target) call(req wire.Request) (*wire.Response, error) {
	resp, err := wire.Call(t.sock, req)
	if err != nil && errors.Is(err, wire.ErrDaemonDown) && !t.vm {
		return resp, fmt.Errorf("%w; start it with `clankerctl smol up`", err)
	}
	return resp, err
}

func guestSocket() string {
	if s := os.Getenv("CLANKERD_GUEST_SOCKET"); s != "" {
		return s
	}
	return wire.GuestSocket
}

func connect(cf *config.Flags) (*target, error) {
	if fi, err := os.Stat(guestSocket()); err == nil && fi.Mode()&os.ModeSocket != 0 {
		return &target{sock: guestSocket(), vm: true, cf: cf}, nil
	}
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

func (c *ctl) warn(resp *wire.Response) {
	if resp == nil {
		return
	}
	for _, w := range resp.Warnings {
		fmt.Fprintln(c.stderr, "warning: "+w)
	}
}

func exports(l wire.Lease) string {
	return fmt.Sprintf("export CLANKER_LEASE_APP_PORT=%d\nexport CLANKER_LEASE_CDP_URL=%s\nexport CLANKER_LEASE_HOSTS='%s'\n",
		l.AppPort, l.CDPURL, strings.Join(l.Hosts, " "))
}

func (c *ctl) printLease(l wire.Lease, asJSON bool) error {
	if asJSON {
		return c.printJSON(l)
	}
	fmt.Fprint(c.stdout, exports(l))
	return nil
}

func (c *ctl) printJSON(v any) error {
	enc := json.NewEncoder(c.stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}

func (c *ctl) leaseOrBrowser(group, sub string, args []string) error {
	fs := flag.NewFlagSet(group+" "+sub, flag.ContinueOnError)
	cf := config.AddFlags(fs)
	asJSON := fs.Bool("json", false, "structured output")
	purge := fs.Bool("purge", false, "also delete the Chrome profile")
	pos, err := parseArgs(fs, args)
	if err != nil {
		return err
	}
	var req wire.Request
	needName := true
	switch group + " " + sub {
	case "lease acquire":
		req.Op = wire.OpLeaseAcquire
	case "lease release":
		req.Op, req.Purge = wire.OpLeaseRelease, *purge
	case "lease show":
		req.Op = wire.OpLeaseShow
	case "lease list":
		req.Op, needName = wire.OpLeaseList, false
	case "browser start":
		req.Op = wire.OpBrowserStart
	case "browser stop":
		req.Op = wire.OpBrowserStop
	default:
		return usageError{"unknown command " + group + " " + sub}
	}
	if needName {
		if len(pos) == 0 {
			return usageError{fmt.Sprintf("%s %s needs a NAME", group, sub)}
		}
		req.Name, pos = pos[0], pos[1:]
	}
	if req.Op == wire.OpLeaseAcquire {
		req.Hosts, pos = pos, nil
	}
	if len(pos) > 0 {
		return usageError{"unexpected arguments: " + strings.Join(pos, " ")}
	}
	if req.Op == wire.OpLeaseAcquire || req.Op == wire.OpBrowserStart {
		if err := wire.ValidateName(req.Name); err != nil {
			return err
		}
	}
	t, err := connect(cf)
	if err != nil {
		return err
	}
	resp, err := t.call(req)
	if err != nil {
		return err
	}
	c.warn(resp)
	switch req.Op {
	case wire.OpLeaseAcquire, wire.OpLeaseShow:
		return c.printLease(*resp.Lease, *asJSON)
	case wire.OpLeaseList:
		return c.printList(resp.Leases, *asJSON)
	case wire.OpLeaseRelease:
		fmt.Fprintln(c.stdout, "released "+req.Name)
	case wire.OpBrowserStart:
		fmt.Fprintf(c.stdout, "chrome running for %s (cdp %s)\n", req.Name, resp.Lease.CDPURL)
	case wire.OpBrowserStop:
		fmt.Fprintln(c.stdout, "chrome stopped for "+req.Name)
	}
	return nil
}

func (c *ctl) printList(ls []wire.Lease, asJSON bool) error {
	if asJSON {
		if ls == nil {
			ls = []wire.Lease{}
		}
		return c.printJSON(ls)
	}
	tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSLOT\tAPP\tCDP\tCHROME\tHOSTS")
	for _, l := range ls {
		chrome := "stopped"
		if l.ChromeRunning {
			chrome = "running"
		}
		fmt.Fprintf(tw, "%s\t%d\t%d\t%d\t%s\t%s\n", l.Name, l.Slot, l.AppPort, l.CDPPort, chrome, strings.Join(l.Hosts, " "))
	}
	return tw.Flush()
}
