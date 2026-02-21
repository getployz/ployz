package runtimecmd

import (
	"ployz/cmd/ployz/cmdutil"

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
