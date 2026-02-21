package cluster

import (
	"github.com/spf13/cobra"
)

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:     "cluster",
		Aliases: []string{"ctx", "context"},
		Short:   "Manage local cluster profiles",
	}

	cmd.AddCommand(listCmd())
	cmd.AddCommand(currentCmd())
	cmd.AddCommand(useCmd())
	cmd.AddCommand(removeCmd())

	set := setCmd()
	set.Hidden = true
	cmd.AddCommand(set)

	return cmd
}
