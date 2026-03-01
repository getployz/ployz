package networkcmd

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/platform"
	"ployz/sdk"

	"github.com/spf13/cobra"
)

func createCmd() *cobra.Command {
	var socketPath string
	var target string

	cmd := &cobra.Command{
		Use:   "create [name]",
		Short: "Create a new network",
		Args:  cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := platform.DefaultNetworkName
			if len(args) > 0 {
				name = args[0]
			}

			dialTarget := socketPath
			var opts []sdk.DialOption
			if target != "" {
				dialTarget = target
			}

			client, err := sdk.Dial(cmd.Context(), dialTarget, opts...)
			if err != nil {
				return fmt.Errorf("connect to daemon: %w", err)
			}
			defer client.Close()

			if err := client.CreateNetwork(cmd.Context(), name); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("Network %s created.", ui.Bold(name)))
			return nil
		},
	}

	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	cmd.Flags().StringVar(&target, "target", "", "Remote daemon (e.g. root@host)")

	return cmd
}
