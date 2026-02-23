package service

import "github.com/spf13/cobra"

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:     "service",
		Aliases: []string{"svc"},
		Short:   "Run and manage cluster services",
	}

	cmd.AddCommand(runCmd())
	return cmd
}
