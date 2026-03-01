package main

import (
	"fmt"
	"os"

	"ployz/cmd/ployz/daemon"
	devcmd "ployz/cmd/ployz/dev"
	"ployz/internal/logging"
	"ployz/internal/support/buildinfo"

	"github.com/spf13/cobra"
)

func main() {
	var debug bool
	if err := logging.Configure(logging.LevelWarn); err != nil {
		_, _ = os.Stderr.WriteString("configure logger: " + err.Error() + "\n")
		os.Exit(1)
	}

	root := &cobra.Command{
		Use:           "ployz",
		Short:         "Container orchestration with overlay networking",
		Version:       buildinfo.Version,
		SilenceErrors: true,
		SilenceUsage:  true,
		PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
			level := logging.LevelWarn
			if debug {
				level = logging.LevelDebug
			}
			return logging.Configure(level)
		},
	}
	root.PersistentFlags().BoolVar(&debug, "debug", false, "Enable debug logging")

	root.AddCommand(devcmd.Cmd())

	daemonCmd := daemon.Cmd()
	daemonCmd.Hidden = true
	root.AddCommand(daemonCmd)

	if err := root.Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
