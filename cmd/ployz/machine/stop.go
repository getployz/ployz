package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func stopCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var purge bool
	var socketPath string

	cmd := &cobra.Command{
		Use:   "stop",
		Short: "Disable network through ployzd",
		RunE: func(cmd *cobra.Command, args []string) error {
			svc, err := service(socketPath)
			if err != nil {
				return err
			}
			if err := svc.Stop(cmd.Context(), nf.Network, purge); err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("disabled network %s", ui.Accent(nf.Network)))
			if purge {
				fmt.Println(ui.Muted("  state purged"))
			}
			return nil
		},
	}

	nf.Bind(cmd)
	cmd.Flags().BoolVar(&purge, "purge", false, "Remove network state after disable")
	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	return cmd
}
