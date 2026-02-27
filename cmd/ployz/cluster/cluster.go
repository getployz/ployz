package cluster

import (
	"github.com/spf13/cobra"
)

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "context",
		Short: "Manage local control-plane contexts",
	}

	cmd.AddCommand(listCmd())
	cmd.AddCommand(currentCmd())
	cmd.AddCommand(useCmd())
	cmd.AddCommand(removeCmd())
	cmd.AddCommand(setCmd())

	return cmd
}
