package agent

import (
	"fmt"
	"time"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"

	"github.com/spf13/cobra"
)

func installCmd() *cobra.Command {
	var (
		dataRoot   string
		socketPath string
	)

	cmd := &cobra.Command{
		Use:   "install",
		Short: "Install and start the ployz agent services",
		RunE: func(cmd *cobra.Command, args []string) error {
			if dataRoot == "" {
				dataRoot = defaults.DataRoot()
			}
			if socketPath == "" {
				socketPath = client.DefaultSocketPath()
			}

			svc := NewPlatformService()
			fmt.Println(ui.InfoMsg("installing agent services"))

			if err := svc.Install(cmd.Context(), InstallConfig{
				DataRoot:   dataRoot,
				SocketPath: socketPath,
			}); err != nil {
				return fmt.Errorf("install agent: %w", err)
			}

			fmt.Println(ui.InfoMsg("waiting for daemon to become ready"))
			if err := WaitReady(cmd.Context(), socketPath, 15*time.Second); err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("agent installed and running"))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("socket", socketPath),
				ui.KV("data root", dataRoot),
				ui.KV("daemon log", cmdutil.DaemonLogPath(dataRoot)),
			))
			return nil
		},
	}

	cmd.Flags().StringVar(&dataRoot, "data-root", "", "Machine data root")
	cmd.Flags().StringVar(&socketPath, "socket", "", "Daemon unix socket path")
	return cmd
}
