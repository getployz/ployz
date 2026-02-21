package daemon

import (
	"os"
	"os/signal"
	"syscall"

	"ployz/internal/daemon/server"
	"ployz/internal/daemon/supervisor"

	"github.com/spf13/cobra"
)

func runCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "run",
		Short: "Run ployzd in foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, cancel := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer cancel()
			mgr, err := supervisor.New(ctx, opts.dataRoot)
			if err != nil {
				return err
			}
			srv := server.New(mgr)
			return srv.ListenAndServe(ctx, opts.socket)
		},
	}
}
