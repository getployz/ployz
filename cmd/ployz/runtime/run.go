package runtimecmd

import (
	"os"
	"os/signal"
	"syscall"

	"ployz/internal/runtime/engine"

	"github.com/spf13/cobra"
)

func runCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "run",
		Short: "Run ployz runtime in foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, cancel := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer cancel()
			return engine.Run(ctx, opts.dataRoot)
		},
	}
}
