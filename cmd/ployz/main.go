package main

import (
	"fmt"
	"os"

	"ployz/cmd/ployz/host"
	"ployz/cmd/ployz/machine"

	"github.com/spf13/cobra"
)

func main() {
	root := &cobra.Command{
		Use:   "ployz",
		Short: "Container orchestration with overlay networking",
	}

	root.AddCommand(machine.Cmd())
	root.AddCommand(host.Cmd())

	if err := root.Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
