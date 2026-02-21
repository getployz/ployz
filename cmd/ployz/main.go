package main

import (
	"fmt"
	"os"

	"ployz/cmd/ployz/agent"
	"ployz/cmd/ployz/cluster"
	"ployz/cmd/ployz/daemon"
	devcmd "ployz/cmd/ployz/dev"
	"ployz/cmd/ployz/host"
	"ployz/cmd/ployz/initcmd"
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

	root.AddCommand(initcmd.Cmd())
	root.AddCommand(node.Cmd())
	root.AddCommand(cluster.Cmd())
	root.AddCommand(host.Cmd())
	root.AddCommand(agent.Cmd())
	root.AddCommand(devcmd.Cmd())

	daemonCmd := daemon.Cmd()
	daemonCmd.Hidden = true
	root.AddCommand(daemonCmd)

	runtimeCmd := runtimecmd.Cmd()
	runtimeCmd.Hidden = true
	root.AddCommand(runtimeCmd)

	if err := root.Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
