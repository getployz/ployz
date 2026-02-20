package main

import (
	"fmt"
	"os"

	"ployz/cmd/ployz/cluster"
	"ployz/cmd/ployz/daemon"
	"ployz/cmd/ployz/host"
	"ployz/cmd/ployz/node"

	"github.com/spf13/cobra"
)

func main() {
	root := &cobra.Command{
		Use:   "ployz",
		Short: "Container orchestration with overlay networking",
	}

	root.AddCommand(initCmd())
	root.AddCommand(node.Cmd())
	root.AddCommand(cluster.Cmd())
	root.AddCommand(host.Cmd())

	daemonCmd := daemon.Cmd()
	daemonCmd.Hidden = true
	root.AddCommand(daemonCmd)

	if err := root.Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
