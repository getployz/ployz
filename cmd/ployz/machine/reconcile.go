package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	machinelib "ployz/internal/machine"

	"github.com/spf13/cobra"
)

func reconcileCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags

	cmd := &cobra.Command{
		Use:   "reconcile",
		Short: "Reconcile WireGuard peers from Corrosion machines table",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctrl, err := machinelib.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			count, err := ctrl.Reconcile(cmd.Context(), nf.Config())
			if err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("reconciled %d peers for network %s", count, ui.Accent(nf.Network)))
			return nil
		},
	}

	nf.Bind(cmd)
	return cmd
}
