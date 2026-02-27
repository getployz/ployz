package node

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func removeCmd() *cobra.Command {
	var cf cmdutil.ContextFlags

	cmd := &cobra.Command{
		Use:     "remove <id-or-endpoint>",
		Aliases: []string{"rm"},
		Short:   "Remove a machine",
		Args:    cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			contextName, svc, _, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			if err := svc.RemoveMachine(cmd.Context(), args[0]); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("removed machine %s via context %s", ui.Accent(args[0]), ui.Accent(contextName)))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}
