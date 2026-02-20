package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	machinelib "ployz/internal/machine"

	"github.com/spf13/cobra"
)

func removeCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags

	cmd := &cobra.Command{
		Use:     "remove <machine-id-or-endpoint>",
		Aliases: []string{"rm"},
		Short:   "Remove a machine from the network",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			ctrl, err := machinelib.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			if err := ctrl.RemoveMachine(cmd.Context(), nf.Config(), args[0]); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("removed machine %s from network %s", ui.Accent(args[0]), ui.Accent(nf.Network)))
			return nil
		},
	}

	nf.Bind(cmd)
	return cmd
}
