package configure

import (
	"fmt"

	"ployz/internal/adapter/wireguard"

	"github.com/spf13/cobra"
)

func Cmd() *cobra.Command {
	var (
		tunSocketPath  string
		privSocketPath string
		mtu            int
	)

	cmd := &cobra.Command{
		Use:   "configure",
		Short: "Provision macOS WireGuard tunnel descriptor",
		Long:  "Starts the privileged macOS network helper, creates a utun device, and passes its file descriptor to the daemon. Run once per boot with sudo.",
		RunE: func(cmd *cobra.Command, args []string) error {
			if err := runConfigure(cmd.Context(), tunSocketPath, privSocketPath, mtu); err != nil {
				return err
			}
			fmt.Println("configured")
			return nil
		},
	}

	cmd.Flags().StringVar(&tunSocketPath, "tun-socket", wireguard.DefaultTUNSocketPath(), "Unix socket path used for TUN fd passing")
	cmd.Flags().StringVar(&privSocketPath, "priv-socket", wireguard.DefaultPrivilegedSocketPath(), "Unix socket path used by privileged helper")
	cmd.Flags().IntVar(&mtu, "mtu", 1280, "TUN MTU to request")

	return cmd
}
