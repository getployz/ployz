package node

import "github.com/spf13/cobra"

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:     "node",
		Aliases: []string{"machine"},
		Short:   "Manage cluster nodes",
	}
	cmd.AddCommand(addCmd())
	cmd.AddCommand(listCmd())
	cmd.AddCommand(removeCmd())
	cmd.AddCommand(statusCmd())
	cmd.AddCommand(doctorCmd())
	return cmd
}
