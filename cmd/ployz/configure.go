package main

import (
	"fmt"

	"github.com/spf13/cobra"
)

func configureCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "configure",
		Short: "Configure host integration (placeholder)",
		RunE: func(cmd *cobra.Command, args []string) error {
			return fmt.Errorf("configure command is not implemented in this snapshot")
		},
	}
}
