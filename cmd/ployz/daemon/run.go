package daemon

import (
	"fmt"
	"os"
	"os/signal"
	"syscall"

	daemonruntime "ployz/daemon"
	"ployz/machine"
	"ployz/platform"

	"github.com/spf13/cobra"
)

func runCmd(opts *options) *cobra.Command {
	return &cobra.Command{
		Use:   "run",
		Short: "Run ployzd in foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, cancel := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer cancel()

			m, err := machine.New(opts.dataRoot)
			if err != nil {
				return fmt.Errorf("create machine: %w", err)
			}

			builder, err := platform.NewMeshBuilder(m.Identity(), opts.dataRoot)
			if err != nil {
				return fmt.Errorf("create mesh builder: %w", err)
			}

			return daemonruntime.Run(ctx, m, builder, opts.socket)
		},
	}
}
