package devcmd

import (
	"fmt"
	"log/slog"
	"os"
	"os/signal"
	"syscall"

	"ployz/cmd/ployz/cmdutil"
	daemonruntime "ployz/daemon"
	"ployz/machine"
	"ployz/platform"

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

			m, err := machine.New(dataRoot)
			if err != nil {
				return fmt.Errorf("create machine: %w", err)
			}

			builder, err := platform.NewMeshBuilder(m.Identity(), dataRoot)
			if err != nil {
				return fmt.Errorf("create mesh builder: %w", err)
			}

			slog.Info("daemon listening", "socket", socketPath)
			return daemonruntime.Run(ctx, m, builder, socketPath)
		},
	}

	cmd.Flags().StringVar(&socketPath, "socket", platform.DaemonSocketPath, "ployzd unix socket path")
	cmd.Flags().StringVar(&dataRoot, "data-root", platform.DaemonDataRoot, "Machine data root")

	return cmd
}
