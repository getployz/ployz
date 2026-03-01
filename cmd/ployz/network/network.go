package networkcmd

import "github.com/spf13/cobra"

// Cmd returns the parent "ployz network" command.
func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "network",
		Short: "Manage networks",
	}

	cmd.AddCommand(createCmd())
	return cmd
}
