package host

import (
	"ployz/cmd/ployz/host/access"

	"github.com/spf13/cobra"
)

func Cmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "host",
		Short: "Host-level access helpers",
	}
	cmd.AddCommand(access.Cmd())
	return cmd
}
