package networkcmd

import (
	"context"
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/platform"

	"github.com/spf13/cobra"
)

func createCmd(hostFlag, contextFlag *string) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "create [name]",
		Short: "Create a new network",
		Args:  cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			name := platform.DefaultNetworkName
			if len(args) > 0 {
				name = args[0]
			}

			err := ui.RunWithSpinner(cmd.Context(), fmt.Sprintf("Creating network %s", ui.Bold(name)), func(ctx context.Context) error {
				client, err := cmdutil.Connect(ctx, *hostFlag, *contextFlag)
				if err != nil {
					return fmt.Errorf("connect to daemon: %w", err)
				}
				defer client.Close()

				return client.CreateNetwork(ctx, name)
			})
			if err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("Network %s created.", ui.Bold(name)))
			return nil
		},
	}

	return cmd
}
