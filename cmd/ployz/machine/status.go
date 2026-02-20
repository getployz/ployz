package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	machinelib "ployz/internal/machine"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags

	cmd := &cobra.Command{
		Use:   "status",
		Short: "Show local machine runtime status",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctrl, err := machinelib.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			status, err := ctrl.Status(cmd.Context(), nf.Config())
			if err != nil {
				return err
			}
			fmt.Print(ui.KeyValues("",
				ui.KV("configured", ui.Bool(status.Configured)),
				ui.KV("running", ui.Bool(status.Running)),
				ui.KV("wireguard", ui.Bool(status.WireGuard)),
				ui.KV("corrosion", ui.Bool(status.Corrosion)),
				ui.KV("docker", ui.Bool(status.DockerNet)),
				ui.KV("state", status.StatePath),
			))
			return nil
		},
	}

	nf.Bind(cmd)
	return cmd
}
