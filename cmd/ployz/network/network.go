package network

import "github.com/spf13/cobra"

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "network",
		Short: "Manage cluster networks",
	}
	cmd.AddCommand(listCmd())
	cmd.AddCommand(statusCmd())
	return cmd
}
