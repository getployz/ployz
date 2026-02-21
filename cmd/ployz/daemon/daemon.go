package daemon

import (
	"os"
	"os/signal"
	"syscall"

	"ployz/cmd/ployz/cmdutil"
	"ployz/internal/daemon/server"
	"ployz/internal/daemon/supervisor"

	"github.com/spf13/cobra"
)

type options struct {
	socket   string
	dataRoot string
}

func Cmd() *cobra.Command {
	opts := &options{}

	cmd := &cobra.Command{
		Use:   "daemon",
		Short: "Manage local ployzd lifecycle",
	}

	cmd.PersistentFlags().StringVar(&opts.socket, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	cmd.PersistentFlags().StringVar(&opts.dataRoot, "data-root", cmdutil.DefaultDataRoot(), "Machine data root")

	cmd.AddCommand(runCmd(opts))
	return cmd
}

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
