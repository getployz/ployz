package contextcmd

import (
	"fmt"

	"ployz/cmd/ployz/ui"
	"ployz/config"

	"github.com/spf13/cobra"
)

func useCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "use <name>",
		Short: "Set the current context",
		Args:  cobra.ExactArgs(1),
		RunE: func(_ *cobra.Command, args []string) error {
			name := args[0]

			cfg, err := config.Load()
			if err != nil {
				return err
			}
			if err := cfg.Use(name); err != nil {
				return err
			}
			if err := cfg.Save(); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("Switched to context %s.", ui.Bold(name)))
			return nil
		},
	}
}
