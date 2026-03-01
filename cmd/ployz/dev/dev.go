package devcmd

import (
	"log/slog"
	"os"
	"os/signal"
	"syscall"

	"ployz/cmd/ployz/cmdutil"
	daemonruntime "ployz/daemon"

	"github.com/spf13/cobra"
)

func Cmd() *cobra.Command {
	var socketPath string
	var dataRoot string

	cmd := &cobra.Command{
		Use:   "dev",
		Short: "Run daemon in the foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()

			if cmdutil.IsDaemonRunning(cmd.Context(), socketPath) {
				slog.Warn("daemon already running; skipping foreground start", "socket", socketPath)
				<-ctx.Done()
				return nil
			}

			slog.Info("daemon listening", "socket", socketPath)
			return daemonruntime.Run(ctx, dataRoot, socketPath)
		},
	}

	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	cmd.Flags().StringVar(&dataRoot, "data-root", cmdutil.DefaultDataRoot(), "Machine data root")

	return cmd
}
