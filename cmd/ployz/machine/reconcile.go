package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func reconcileCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var socketPath string

	cmd := &cobra.Command{
		Use:   "reconcile",
		Short: "Trigger peer reconcile through ployzd",
		RunE: func(cmd *cobra.Command, args []string) error {
			svc, err := service(socketPath)
			if err != nil {
				return err
			}
			if err := svc.Reconcile(cmd.Context(), nf.Network); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("triggered reconcile for network %s", ui.Accent(nf.Network)))
			return nil
		},
	}

	nf.Bind(cmd)
	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	return cmd
}
