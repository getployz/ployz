package main

import (
	"fmt"

	"github.com/spf13/cobra"
)

func devCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "dev",
		Short: "Start a dev environment (placeholder)",
		RunE: func(cmd *cobra.Command, args []string) error {
			return fmt.Errorf("dev command is not implemented in this snapshot")
		},
	}
}
