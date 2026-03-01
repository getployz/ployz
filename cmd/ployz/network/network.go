package networkcmd

import "github.com/spf13/cobra"

// Cmd returns the parent "ployz network" command. hostFlag and contextFlag
// are pointers to the root persistent flag values.
func Cmd(hostFlag, contextFlag *string) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "network",
		Short: "Manage networks",
	}

	cmd.AddCommand(createCmd(hostFlag, contextFlag))
	return cmd
}
