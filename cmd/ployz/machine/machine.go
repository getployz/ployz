package machine

import "github.com/spf13/cobra"

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "machine",
		Short: "Manage machine runtime and membership",
	}
	cmd.AddCommand(startCmd())
	cmd.AddCommand(stopCmd())
	cmd.AddCommand(statusCmd())
	cmd.AddCommand(addCmd())
	cmd.AddCommand(listCmd())
	cmd.AddCommand(removeCmd())
	cmd.AddCommand(reconcileCmd())
	cmd.AddCommand(doctorCmd())
	return cmd
}
