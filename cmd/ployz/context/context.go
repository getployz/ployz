package contextcmd

import "github.com/spf13/cobra"

// Cmd returns the parent "ployz context" command.
func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "context",
		Short: "Manage daemon contexts",
	}

	cmd.AddCommand(listCmd())
	cmd.AddCommand(useCmd())
	cmd.AddCommand(addCmd())
	cmd.AddCommand(removeCmd())
	return cmd
}
