package network

import (
	initcmd "ployz/cmd/ployz/initcmd"

	"github.com/spf13/cobra"
)

func createCmd() *cobra.Command {
	cmd := initcmd.Cmd()
	cmd.Use = "create [name] [user@host]"
	cmd.Short = "Create or refresh a network context"
	cmd.Long = "Creates or refreshes a network context, applies local runtime settings, and optionally adds a remote machine."
	return cmd
}
