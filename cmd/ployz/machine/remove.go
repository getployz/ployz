package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func removeCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var socketPath string

	cmd := &cobra.Command{
		Use:     "remove <machine-id-or-endpoint>",
		Aliases: []string{"rm"},
		Short:   "Remove a machine from the network",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			svc, err := service(socketPath)
			if err != nil {
				return err
			}
			if err := svc.RemoveMachine(cmd.Context(), nf.Network, args[0]); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("removed machine %s from network %s", ui.Accent(args[0]), ui.Accent(nf.Network)))
			return nil
		},
	}

	nf.Bind(cmd)
	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	return cmd
}
