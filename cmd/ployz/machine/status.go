package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var socketPath string

	cmd := &cobra.Command{
		Use:   "status",
		Short: "Show network status from ployzd",
		RunE: func(cmd *cobra.Command, args []string) error {
			svc, err := service(socketPath)
			if err != nil {
				return err
			}
			status, err := svc.Status(cmd.Context(), nf.Network)
			if err != nil {
				return err
			}
			fmt.Print(ui.KeyValues("",
				ui.KV("configured", ui.Bool(status.Configured)),
				ui.KV("running", ui.Bool(status.Running)),
				ui.KV("wireguard", ui.Bool(status.WireGuard)),
				ui.KV("corrosion", ui.Bool(status.Corrosion)),
				ui.KV("docker", ui.Bool(status.DockerNet)),
				ui.KV("worker", ui.Bool(status.WorkerRunning)),
				ui.KV("state", status.StatePath),
			))
			return nil
		},
	}

	nf.Bind(cmd)
	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	return cmd
}
