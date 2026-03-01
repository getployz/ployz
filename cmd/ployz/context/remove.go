package contextcmd

import (
	"fmt"

	"ployz/cmd/ployz/ui"
	"ployz/config"

	"github.com/spf13/cobra"
)

func removeCmd() *cobra.Command {
	var yes bool

	cmd := &cobra.Command{
		Use:     "remove <name>",
		Short:   "Remove a context",
		Aliases: []string{"rm"},
		Args:    cobra.ExactArgs(1),
		RunE: func(_ *cobra.Command, args []string) error {
			name := args[0]

			if !yes {
				confirmed, err := ui.Confirm(
					fmt.Sprintf("Remove context %s?", ui.Bold(name)),
					"use --yes to skip",
				)
				if err != nil {
					return err
				}
				if !confirmed {
					return nil
				}
			}

			cfg, err := config.Load()
			if err != nil {
				return err
			}
			if err := cfg.Remove(name); err != nil {
				return err
			}
			if err := cfg.Save(); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("Context %s removed.", ui.Bold(name)))
			return nil
		},
	}

	cmd.Flags().BoolVarP(&yes, "yes", "y", false, "Skip confirmation prompt")
	return cmd
}
