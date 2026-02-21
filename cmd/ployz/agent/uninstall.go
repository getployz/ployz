package agent

import (
	"fmt"

	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func uninstallCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "uninstall",
		Short: "Stop and remove the ployz agent services",
		RunE: func(cmd *cobra.Command, args []string) error {
			svc := NewPlatformService()

			if err := svc.Uninstall(cmd.Context()); err != nil {
				return fmt.Errorf("uninstall agent: %w", err)
			}

			fmt.Println(ui.SuccessMsg("agent services removed"))
			return nil
		},
	}
}
