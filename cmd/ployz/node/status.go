package node

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags

	cmd := &cobra.Command{
		Use:   "status",
		Short: "Show cluster node status",
		RunE: func(cmd *cobra.Command, args []string) error {
			_, svc, cl, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			status, err := svc.Status(cmd.Context(), cl.Network)
			if err != nil {
				return err
			}

			ntpDisplay := ui.Bool(status.ClockHealth.NTPHealthy)
			if status.ClockHealth.NTPError != "" {
				ntpDisplay = ui.Warn(status.ClockHealth.NTPError)
			} else if status.ClockHealth.NTPOffsetMs != 0 {
				ntpDisplay = fmt.Sprintf("%s (offset %.1fms)", ntpDisplay, status.ClockHealth.NTPOffsetMs)
			}

			fmt.Print(ui.KeyValues("",
				ui.KV("configured", ui.Bool(status.Configured)),
				ui.KV("running", ui.Bool(status.Running)),
				ui.KV("wireguard", ui.Bool(status.WireGuard)),
				ui.KV("corrosion", ui.Bool(status.Corrosion)),
				ui.KV("docker", ui.Bool(status.DockerNet)),
				ui.KV("convergence", ui.Bool(status.WorkerRunning)),
				ui.KV("clock sync", ntpDisplay),
				ui.KV("state", status.StatePath),
			))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}
