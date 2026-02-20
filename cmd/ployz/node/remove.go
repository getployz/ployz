package node

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func removeCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags

	cmd := &cobra.Command{
		Use:     "remove <id-or-endpoint>",
		Aliases: []string{"rm"},
		Short:   "Remove a node from the cluster",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			clusterName, svc, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}
			_, cl, _ := cf.Resolve()

			if err := svc.RemoveMachine(cmd.Context(), cl.Network, args[0]); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("removed node %s from cluster %s", ui.Accent(args[0]), ui.Accent(clusterName)))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}
