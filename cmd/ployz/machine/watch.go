package machine

import (
	"fmt"
	"time"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	machinelib "ployz/internal/machine"

	"github.com/spf13/cobra"
)

func watchCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var interval time.Duration

	cmd := &cobra.Command{
		Use:   "watch",
		Short: "Continuously reconcile peers from Corrosion",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctrl, err := machinelib.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			cfg := nf.Config()
			ticker := time.NewTicker(interval)
			defer ticker.Stop()

			fmt.Println(ui.InfoMsg("watching network %s (interval %s)", ui.Accent(nf.Network), interval))
			for {
				count, rErr := ctrl.Reconcile(cmd.Context(), cfg)
				if rErr != nil {
					fmt.Println(ui.ErrorMsg("reconcile: %v", rErr))
				} else {
					fmt.Println(ui.SuccessMsg("reconciled %d peers", count))
				}

				select {
				case <-cmd.Context().Done():
					return cmd.Context().Err()
				case <-ticker.C:
				}
			}
		},
	}

	nf.Bind(cmd)
	cmd.Flags().DurationVar(&interval, "interval", 3*time.Second, "Reconcile interval")
	return cmd
}
