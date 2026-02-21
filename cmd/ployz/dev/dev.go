package devcmd

import (
	"fmt"
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
		Short: "Run daemon and runtime in the foreground",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()

			if cmdutil.IsDaemonRunning(cmd.Context(), socketPath) {
				fmt.Fprintln(os.Stderr, "[ployz] agent already running, skipping daemon/runtime")
				<-ctx.Done()
				return nil
			}

			mgr, err := supervisor.New(ctx, dataRoot)
			if err != nil {
				return err
			}
			srv := server.New(mgr)

			fmt.Fprintf(os.Stderr, "[ployz] daemon listening on %s\n", socketPath)
			fmt.Fprintf(os.Stderr, "[ployz] runtime started\n")

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

	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	cmd.Flags().StringVar(&dataRoot, "data-root", cmdutil.DefaultDataRoot(), "Machine data root")

	return cmd
}
