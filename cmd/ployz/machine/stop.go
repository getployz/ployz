package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	machinelib "ployz/internal/machine"

	"github.com/spf13/cobra"
)

func stopCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var purge bool

	cmd := &cobra.Command{
		Use:   "stop",
		Short: "Stop local machine runtime for a network",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctrl, err := machinelib.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			out, err := ctrl.Stop(cmd.Context(), nf.Config(), purge)
			if err != nil {
				return err
			}
			fmt.Println(ui.SuccessMsg("stopped machine for network %s", ui.Accent(out.Network)))
			if purge {
				fmt.Println(ui.Muted("  state purged"))
			}
			return nil
		},
	}

	nf.Bind(cmd)
	cmd.Flags().BoolVar(&purge, "purge", false, "Remove network state directory after stop")
	return cmd
}
