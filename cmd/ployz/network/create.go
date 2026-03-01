package networkcmd

import (
	"context"
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

			var client *sdk.Client

			err := ui.RunWithSpinner(cmd.Context(), fmt.Sprintf("Creating network %s", ui.Bold(name)), func(ctx context.Context) error {
				var err error
				client, err = sdk.Dial(ctx, dialTarget, opts...)
				if err != nil {
					return fmt.Errorf("connect to daemon: %w", err)
				}

				return client.CreateNetwork(ctx, name)
			})
			if err != nil {
				if client != nil {
					client.Close()
				}
				return err
			}
			defer client.Close()

			fmt.Println(ui.SuccessMsg("Network %s created.", ui.Bold(name)))
			return nil
		},
	}

	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	cmd.Flags().StringVar(&target, "target", "", "Remote daemon (e.g. root@host)")

	return cmd
}
