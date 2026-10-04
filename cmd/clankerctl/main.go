package main

import (
	"os"

	"github.com/monai/clankers/sandbox/clanker/internal/cli"
)

func main() { os.Exit(cli.Ctl(os.Args[1:], os.Stdout, os.Stderr)) }
