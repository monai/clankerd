package cli

import (
	"context"
	"flag"
	"fmt"
	"net"
	"os"
	"os/signal"
	"strconv"
	"syscall"

	"github.com/monai/clankers/clankerd/internal/relay"
)

type listFlag []string

func (l *listFlag) String() string     { return fmt.Sprint([]string(*l)) }
func (l *listFlag) Set(v string) error { *l = append(*l, v); return nil }

func runRelay(args []string) error {
	fs := flag.NewFlagSet("relay", flag.ContinueOnError)
	var listens listFlag
	fs.Var(&listens, "listen", "address:port to listen on (repeat for IPv4 and IPv6)")
	target := fs.String("target", "", "host:port to forward to; host 'gateway' means the VM's default gateways")
	pidfile := fs.String("pidfile", "", "pidfile to write once listening")
	if _, err := parseArgs(fs, args); err != nil {
		return err
	}
	if len(listens) == 0 || *target == "" {
		return usageError{"relay needs --listen and --target"}
	}
	host, port, err := net.SplitHostPort(*target)
	if err != nil {
		return err
	}
	dial := relay.Target(*target)
	if host == "gateway" {
		gws, err := defaultGateways()
		if err != nil {
			return err
		}
		dial = relay.HappyEyeballs(gws, port)
	}
	var lns []net.Listener
	for _, l := range listens {
		ln, err := net.Listen("tcp", l)
		if err != nil {
			fmt.Fprintln(os.Stderr, "relay:", err)
			continue
		}
		lns = append(lns, ln)
	}
	if len(lns) == 0 {
		return fmt.Errorf("could not listen on any of %v", []string(listens))
	}
	if *pidfile != "" {
		if err := os.WriteFile(*pidfile, []byte(strconv.Itoa(os.Getpid())+"\n"), 0o644); err != nil {
			return err
		}
		defer os.Remove(*pidfile)
	}
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	done := make(chan struct{}, len(lns))
	for _, ln := range lns {
		go func() { relay.Serve(ctx, ln, dial); done <- struct{}{} }()
	}
	for range lns {
		<-done
	}
	return nil
}
