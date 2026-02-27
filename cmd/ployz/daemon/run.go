package daemon

import (
	"os"
	"os/signal"
	"syscall"

	daemonruntime "ployz/internal/daemon"

	"github.com/spf13/cobra"
)

func runCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "run",
		Short: "Run ployzd in foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, cancel := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer cancel()
			return daemonruntime.Run(ctx, opts.dataRoot, opts.socket)
		},
	}
}
