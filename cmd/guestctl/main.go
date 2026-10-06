package main

import (
	"os"

	"github.com/monai/clankers/clankerd/internal/cli"
)

func main() { os.Exit(cli.Guest(os.Args[1:], os.Stdout, os.Stderr)) }
