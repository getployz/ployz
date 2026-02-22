package main

import (
	"log/slog"
	"os"

	"ployz/cmd/ployz/agent"
	"ployz/cmd/ployz/cluster"
	configurecmd "ployz/cmd/ployz/configure"
	"ployz/cmd/ployz/daemon"
	devcmd "ployz/cmd/ployz/dev"
	"ployz/cmd/ployz/host"
	"ployz/cmd/ployz/initcmd"
	"ployz/cmd/ployz/node"
	"ployz/internal/buildinfo"
	"ployz/internal/logging"

	"github.com/spf13/cobra"
)

func main() {
	var debug bool
	if err := logging.Configure(logging.LevelWarn); err != nil {
		_, _ = os.Stderr.WriteString("configure logger: " + err.Error() + "\n")
		os.Exit(1)
	}

	root := &cobra.Command{
		Use:     "ployz",
		Short:   "Container orchestration with overlay networking",
		Version: buildinfo.Version,
		PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
			level := logging.LevelWarn
			if debug {
				level = logging.LevelDebug
			}
			return logging.Configure(level)
		},
	}
	root.PersistentFlags().BoolVar(&debug, "debug", false, "Enable debug logging")

	root.AddCommand(initcmd.Cmd())
	root.AddCommand(configurecmd.Cmd())
	root.AddCommand(node.Cmd())
	root.AddCommand(cluster.Cmd())
	root.AddCommand(host.Cmd())
	root.AddCommand(agent.Cmd())
	root.AddCommand(devcmd.Cmd())

	daemonCmd := daemon.Cmd()
	daemonCmd.Hidden = true
	root.AddCommand(daemonCmd)

	if err := root.Execute(); err != nil {
		slog.Error("command failed", "err", err)
		os.Exit(1)
	}
}
