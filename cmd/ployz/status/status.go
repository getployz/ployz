package statuscmd

import (
	"context"
	"fmt"

	ployz "ployz"
	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/sdk"

	"github.com/spf13/cobra"
)

// Cmd returns the "ployz status" command. hostFlag and contextFlag are
// pointers to the root persistent flag values.
func Cmd(hostFlag, contextFlag *string) *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show daemon status",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			var (
				client *sdk.Client
				m      *ployz.Machine
			)

			err := ui.RunWithSpinner(cmd.Context(), "Connecting", func(ctx context.Context) error {
				var err error
				client, err = cmdutil.Connect(ctx, *hostFlag, *contextFlag)
				if err != nil {
					return err
				}

				m, err = client.Status(ctx)
				return err
			})
			if err != nil {
				return err
			}
			defer client.Close()

			fmt.Println(ui.KeyValues("  ",
				ui.KV("Name", m.Name),
				ui.KV("Public Key", m.PublicKey),
				ui.KV("Network", m.NetworkPhase),
				ui.KV("Version", m.Version),
			))
			return nil
		},
	}
}
