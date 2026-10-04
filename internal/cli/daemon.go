package cli

import (
	"context"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/signal"
	"syscall"

	"github.com/monai/clankers/sandbox/clankerd/internal/backend"
	"github.com/monai/clankers/sandbox/clankerd/internal/config"
	"github.com/monai/clankers/sandbox/clankerd/internal/daemon"
	"github.com/monai/clankers/sandbox/clankerd/internal/wire"
)

// Daemon runs clankerd and returns its exit code.
func Daemon(args []string, stdout, stderr io.Writer) int {
	if len(args) == 0 || (args[0] != "run" && args[0] != "version") {
		fmt.Fprintln(stderr, "usage: clankerd run [flags]   run the daemon in the foreground\n       clankerd version")
		return 64
	}
	if args[0] == "version" {
		fmt.Fprintf(stdout, "clankerd %s (protocol %d)\n", Version, wire.Version)
		return 0
	}
	fs := flag.NewFlagSet("run", flag.ContinueOnError)
	cf := config.AddFlags(fs)
	if _, err := parseArgs(fs, args[1:]); err != nil {
		fmt.Fprintln(stderr, "clankerd:", err)
		return 64
	}
	if err := runDaemon(cf, stderr); err != nil {
		fmt.Fprintln(stderr, "clankerd:", err)
		return 1
	}
	return 0
}

func runDaemon(cf *config.Flags, stderr io.Writer) error {
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}
	cfg, err := config.Load(cf, cwd)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(cfg.Dirs.State, 0o700); err != nil {
		return err
	}
	f, err := os.OpenFile(cfg.Dirs.LogFile(), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	defer f.Close()
	var level slog.Level
	if err := level.UnmarshalText([]byte(cfg.LogLevel)); err != nil {
		return fmt.Errorf("invalid log level %q", cfg.LogLevel)
	}
	log := slog.New(slog.NewTextHandler(io.MultiWriter(f, stderr), &slog.HandlerOptions{Level: level}))

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	return daemon.New(ctx, cfg, backend.NewSmolvm(cfg.VM), log).Serve(ctx)
}
