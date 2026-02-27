package service

import "github.com/spf13/cobra"

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "service",
		Short: "Deploy and manage services",
	}

	deploy := deployCmd()
	list := listCmd()
	status := statusCmd()
	remove := removeCmd()

	cmd.AddCommand(deploy)
	cmd.AddCommand(list)
	cmd.AddCommand(status)
	cmd.AddCommand(remove)
	return cmd
}
