package main

import (
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"syscall"

	daemonruntime "ployz/daemon"
	"ployz/internal/logging"
	"ployz/internal/support/buildinfo"
	"ployz/platform"

	"github.com/spf13/cobra"
)

func main() {
	if err := logging.Configure(logging.LevelInfo); err != nil {
		_, _ = os.Stderr.WriteString("configure logger: " + err.Error() + "\n")
		os.Exit(1)
	}

	if err := rootCmd().Execute(); err != nil {
		slog.Error("command failed", "err", err)
		os.Exit(1)
	}
}

func rootCmd() *cobra.Command {
	var socketPath string
	var dataRoot string
	var debug bool

	cmd := &cobra.Command{
		Use:     "ployzd",
		Short:   "Ployz network daemon",
		Version: buildinfo.Version,
		PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
			level := logging.LevelInfo
			if debug {
				level = logging.LevelDebug
			}
			return logging.Configure(level)
		},
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()

			m, err := platform.NewMachine(dataRoot)
			if err != nil {
				return fmt.Errorf("create machine: %w", err)
			}

			return daemonruntime.Run(ctx, m, socketPath)
		},
	}

	cmd.PersistentFlags().BoolVar(&debug, "debug", false, "Enable debug logging")
	cmd.Flags().StringVar(&socketPath, "socket", platform.DaemonSocketPath, "Unix socket path")
	cmd.Flags().StringVar(&dataRoot, "data-root", platform.DaemonDataRoot, "Machine data root")
	cmd.AddCommand(dialStdioCmd())
	return cmd
}
