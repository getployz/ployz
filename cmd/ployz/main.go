package main

import (
	"fmt"
	"os"

	contextcmd "ployz/cmd/ployz/context"
	"ployz/cmd/ployz/daemon"
	devcmd "ployz/cmd/ployz/dev"
	networkcmd "ployz/cmd/ployz/network"
	statuscmd "ployz/cmd/ployz/status"
	"ployz/internal/logging"
	"ployz/internal/support/buildinfo"

	"github.com/spf13/cobra"
)

func main() {
	var (
		debug       bool
		host        string
		contextName string
	)
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
			if err := logging.Configure(level); err != nil {
				return err
			}

			// Resolve hidden --host aliases: first one set wins.
			if host == "" {
				for _, alias := range []string{"connect", "server", "target"} {
					if v, _ := cmd.Flags().GetString(alias); v != "" {
						host = v
						break
					}
				}
			}
			return nil
		},
	}
	root.PersistentFlags().BoolVar(&debug, "debug", false, "Enable debug logging")

	// Connection flags — available to all subcommands.
	root.PersistentFlags().StringVar(&host, "host", "", "Connect directly to socket path or user@host")
	root.PersistentFlags().StringVar(&contextName, "context", "", "Context name to use")

	// Hidden aliases for --host so any first guess hits.
	for _, alias := range []string{"connect", "server", "target"} {
		root.PersistentFlags().String(alias, "", "")
		_ = root.PersistentFlags().MarkHidden(alias)
	}

	root.AddCommand(devcmd.Cmd())
	root.AddCommand(networkcmd.Cmd())
	root.AddCommand(contextcmd.Cmd())
	root.AddCommand(statuscmd.Cmd(&host, &contextName))

	daemonCmd := daemon.Cmd()
	daemonCmd.Hidden = true
	root.AddCommand(daemonCmd)

	if err := root.Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
