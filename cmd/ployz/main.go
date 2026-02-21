package main

import (
	"fmt"
	"os"

	"ployz/cmd/ployz/cluster"
	"ployz/cmd/ployz/daemon"
	devcmd "ployz/cmd/ployz/dev"
	"ployz/cmd/ployz/host"
	"ployz/cmd/ployz/node"
	runtimecmd "ployz/cmd/ployz/runtime"
	"ployz/internal/buildinfo"

	"github.com/spf13/cobra"
)

func main() {
	root := &cobra.Command{
		Use:     "ployz",
		Short:   "Container orchestration with overlay networking",
		Version: buildinfo.Version,
	}

	root.AddCommand(initCmd())
	root.AddCommand(node.Cmd())
	root.AddCommand(cluster.Cmd())
	root.AddCommand(host.Cmd())
	root.AddCommand(runtimecmd.Cmd())
	root.AddCommand(devcmd.Cmd())

	daemonCmd := daemon.Cmd()
	daemonCmd.Hidden = true
	root.AddCommand(daemonCmd)

	if err := root.Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
