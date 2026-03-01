package daemon

import (
	"ployz/cmd/ployz/cmdutil"

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
