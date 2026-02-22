package main

import (
	"log/slog"
	"os"
	"os/signal"
	"syscall"

	"ployz/internal/buildinfo"
	"ployz/internal/daemon/server"
	"ployz/internal/daemon/supervisor"
	"ployz/internal/logging"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"

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
			mgr, err := supervisor.New(ctx, dataRoot)
			if err != nil {
				return err
			}
			srv := server.New(mgr)
			return srv.ListenAndServe(ctx, socketPath)
		},
	}

	cmd.PersistentFlags().BoolVar(&debug, "debug", false, "Enable debug logging")
	cmd.Flags().StringVar(&socketPath, "socket", client.DefaultSocketPath(), "Unix socket path")
	cmd.Flags().StringVar(&dataRoot, "data-root", defaults.DataRoot(), "Machine data root")
	cmd.AddCommand(dialStdioCmd())
	return cmd
}
