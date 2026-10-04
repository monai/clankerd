package cli

import (
	"bufio"
	"context"
	"flag"
	"fmt"
	"net"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"

	"github.com/monai/clankers/sandbox/clanker/internal/relay"
)

// runRelay is the hidden VM-side forwarder: one process per lease, started detached by clankerd.
// It binds first, then records its pid, so a second start that loses the race exits quietly.
func runRelay(args []string) error {
	fs := flag.NewFlagSet("relay", flag.ContinueOnError)
	listen := fs.String("listen", "", "address to listen on")
	target := fs.String("target", "", "address to forward to; host may be 'gateway' for the default gateway")
	pidfile := fs.String("pidfile", "", "pidfile to write once listening")
	if _, err := parseArgs(fs, args); err != nil {
		return err
	}
	if *listen == "" || *target == "" {
		return usageError{"relay needs --listen and --target"}
	}
	host, port, err := net.SplitHostPort(*target)
	if err != nil {
		return err
	}
	if host == "gateway" {
		if host, err = defaultGateway(); err != nil {
			return err
		}
	}
	ln, err := net.Listen("tcp", *listen)
	if err != nil {
		return err
	}
	if *pidfile != "" {
		if err := os.WriteFile(*pidfile, []byte(strconv.Itoa(os.Getpid())+"\n"), 0o644); err != nil {
			return err
		}
		defer os.Remove(*pidfile)
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	relay.Serve(ctx, ln, net.JoinHostPort(host, port))
	return nil
}

// defaultGateway reads the VM's default route.
func defaultGateway() (string, error) {
	f, err := os.Open("/proc/net/route")
	if err != nil {
		return "", fmt.Errorf("cannot find the host address (no /proc/net/route); set CLANKERD_HOST_ADDR: %w", err)
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		fields := strings.Fields(sc.Text())
		if len(fields) < 3 || fields[1] != "00000000" {
			continue
		}
		g, err := strconv.ParseUint(fields[2], 16, 32)
		if err != nil {
			continue
		}
		return net.IPv4(byte(g), byte(g>>8), byte(g>>16), byte(g>>24)).String(), nil
	}
	return "", fmt.Errorf("no default route; set CLANKERD_HOST_ADDR")
}
