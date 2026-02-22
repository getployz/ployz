package daemon

import (
	"fmt"
	"os"

	"ployz/internal/adapter/wireguard"

	"github.com/spf13/cobra"
)

func privHelperCmd() *cobra.Command {
	var socketPath string
	var token string

	cmd := &cobra.Command{
		Use:    "priv-helper",
		Short:  "Run privileged network helper",
		Hidden: true,
		RunE: func(cmd *cobra.Command, args []string) error {
			if os.Geteuid() != 0 {
				return fmt.Errorf("priv-helper requires root")
			}
			return wireguard.RunPrivilegedHelper(cmd.Context(), socketPath, token)
		},
	}

	cmd.Flags().StringVar(&socketPath, "socket", wireguard.DefaultPrivilegedSocketPath(), "Privileged helper unix socket path")
	cmd.Flags().StringVar(&token, "token", "", "Shared secret for helper requests")
	_ = cmd.MarkFlagRequired("token")

	return cmd
}
