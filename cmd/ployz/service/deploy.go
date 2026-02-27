package service

import "github.com/spf13/cobra"

func deployCmd() *cobra.Command {
	cmd := runCmd()
	cmd.Use = "deploy <name>"
	cmd.Short = "Deploy or update a single-image service"
	cmd.Aliases = []string{"run"}
	return cmd
}
