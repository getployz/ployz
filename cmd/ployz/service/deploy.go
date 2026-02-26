package service

import "github.com/spf13/cobra"

func DeployCmd() *cobra.Command {
	cmd := runCmd()
	cmd.Use = "deploy <name>"
	cmd.Short = "Deploy or update a single-image service"
	cmd.Aliases = nil
	return cmd
}
