package main

import (
	"context"
	"fmt"
	"os"

	"ployz/cmd/ployz/agent"
	"ployz/cmd/ployz/cluster"
	configurecmd "ployz/cmd/ployz/configure"
	"ployz/cmd/ployz/daemon"
	devcmd "ployz/cmd/ployz/dev"
	"ployz/cmd/ployz/network"
	"ployz/cmd/ployz/node"
	"ployz/cmd/ployz/service"
	"ployz/internal/support/buildinfo"
	"ployz/internal/support/logging"

	"github.com/spf13/cobra"
	"go.opentelemetry.io/otel"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
)

func main() {
	tp := sdktrace.NewTracerProvider()
	otel.SetTracerProvider(tp)
	defer func() {
		_ = tp.Shutdown(context.Background())
	}()

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

	root.AddCommand(configurecmd.Cmd())
	root.AddCommand(node.Cmd())
	root.AddCommand(node.StatusCmd())
	root.AddCommand(node.DoctorCmd())
	root.AddCommand(cluster.Cmd())
	root.AddCommand(network.Cmd())
	root.AddCommand(service.Cmd())
	root.AddCommand(agent.Cmd())
	root.AddCommand(devcmd.Cmd())

	daemonCmd := daemon.Cmd()
	daemonCmd.Hidden = true
	root.AddCommand(daemonCmd)

	if err := root.Execute(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
