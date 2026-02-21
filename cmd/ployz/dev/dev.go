package devcmd

import (
	"os"
	"os/signal"
	"syscall"

	"ployz/cmd/ployz/cmdutil"
	"ployz/internal/daemon/server"
	"ployz/internal/daemon/supervisor"
	"ployz/internal/runtime/engine"

	"github.com/spf13/cobra"
	"golang.org/x/sync/errgroup"
)

func Cmd() *cobra.Command {
	var socketPath string
	var dataRoot string

	cmd := &cobra.Command{
		Use:   "dev",
		Short: "Developer commands",
	}

	run := &cobra.Command{
		Use:   "run",
		Short: "Run daemon and runtime together",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()

			mgr, err := supervisor.New(ctx, dataRoot)
			if err != nil {
				return err
			}
			srv := server.New(mgr)

			g, gctx := errgroup.WithContext(ctx)
			g.Go(func() error {
				return srv.ListenAndServe(gctx, socketPath)
			})
			g.Go(func() error {
				return engine.Run(gctx, dataRoot)
			})
			return g.Wait()
		},
	}

	run.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	run.Flags().StringVar(&dataRoot, "data-root", cmdutil.DefaultDataRoot(), "Machine data root")
	cmd.AddCommand(run)

	return cmd
}
