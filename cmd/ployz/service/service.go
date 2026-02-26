package service

import "github.com/spf13/cobra"

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:     "service",
		Aliases: []string{"svc"},
		Short:   "Deploy and manage cluster services",
	}

	deploy := DeployCmd()
	run := runCmd()
	run.Hidden = true
	cmd.AddCommand(deploy)
	cmd.AddCommand(run)
	return cmd
}
