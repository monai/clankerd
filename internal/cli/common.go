// Package cli implements the hostctl, guestctl and clankerd command lines.
package cli

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"strings"
	"text/tabwriter"

	"github.com/monai/clankers/clankerd/internal/wire"
)

var Version = "dev"

type ctl struct {
	name, usage    string
	stdout, stderr io.Writer
}

type usageError struct{ msg string }

func (u usageError) Error() string { return "usage: " + u.msg }

// main runs one command line and returns its exit code.
func (c *ctl) main(run func() error) int {
	err := run()
	if err == nil {
		return 0
	}
	var u usageError
	if errors.As(err, &u) {
		fmt.Fprint(c.stderr, c.usage)
		if u.msg != "" {
			fmt.Fprintln(c.stderr, "\n"+u.msg)
		}
		return 64
	}
	fmt.Fprintf(c.stderr, "%s: %v\n", c.name, err)
	return 1
}

func (c *ctl) version() {
	fmt.Fprintf(c.stdout, "%s %s (protocol %d)\n", c.name, Version, wire.Version)
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

// leaseCmd runs one lease or browser command; allowed lists the "group sub" pairs this binary offers.
// fs carries any flags the caller already registered. sock resolves the daemon socket once the
// arguments are known to be valid; downHint is appended when the daemon is unreachable.
func (c *ctl) leaseCmd(fs *flag.FlagSet, group, sub string, args, allowed []string, sock func() (string, error), downHint string) error {
	cmd := group + " " + sub
	known := false
	for _, a := range allowed {
		known = known || a == cmd
	}
	if !known {
		return usageError{"unknown command " + cmd}
	}
	asJSON := fs.Bool("json", false, "structured output")
	purge := new(bool)
	if cmd == "lease release" {
		fs.BoolVar(purge, "purge", false, "also delete the Chrome profile")
	}
	pos, err := parseArgs(fs, args)
	if err != nil {
		return err
	}
	var req wire.Request
	needName := true
	switch cmd {
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
	}
	if needName {
		if len(pos) == 0 {
			return usageError{cmd + " needs a NAME"}
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
	s, err := sock()
	if err != nil {
		return err
	}
	resp, err := wire.Call(s, req)
	if err != nil {
		if errors.Is(err, wire.ErrDaemonDown) {
			err = fmt.Errorf("%w; %s", err, downHint)
		}
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
