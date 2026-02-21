package runtimecmd

import (
	"os"
	"os/signal"
	"syscall"

	"ployz/cmd/ployz/cmdutil"
	"ployz/internal/runtime/engine"

	"github.com/spf13/cobra"
)

type options struct {
	dataRoot string
}

func Cmd() *cobra.Command {
	opts := &options{}

	cmd := &cobra.Command{
		Use:   "runtime",
		Short: "Manage local ployz runtime lifecycle",
	}

	cmd.PersistentFlags().StringVar(&opts.dataRoot, "data-root", cmdutil.DefaultDataRoot(), "Machine data root")

	cmd.AddCommand(runCmd(opts))
	return cmd
}

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
