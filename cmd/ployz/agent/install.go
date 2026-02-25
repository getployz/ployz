package agent

import (
	"context"
	"fmt"
	"time"

	"ployz/cmd/ployz/ui"
	sdkagent "ployz/pkg/sdk/agent"
	"ployz/pkg/sdk/client"
	"ployz/pkg/sdk/defaults"
	"ployz/pkg/sdk/telemetry"

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

			svc := sdkagent.NewPlatformService()
			telemetryOut := ui.NewTelemetryOutput()
			defer telemetryOut.Close()

			op, err := telemetry.EmitPlan(cmd.Context(), telemetryOut.Tracer("ployz/cmd/agent"), "agent.install", telemetry.Plan{Steps: []telemetry.PlannedStep{
				{ID: "install", Title: "installing agent services"},
				{ID: "wait_ready", Title: "waiting for daemon readiness"},
			}})
			if err != nil {
				return err
			}
			var opErr error
			defer func() {
				op.End(opErr)
			}()

			opErr = op.RunStep(op.Context(), "install", func(stepCtx context.Context) error {
				if installErr := svc.Install(stepCtx, sdkagent.InstallConfig{
					DataRoot:   dataRoot,
					SocketPath: socketPath,
				}); installErr != nil {
					return fmt.Errorf("install agent: %w", installErr)
				}
				return nil
			})
			if opErr != nil {
				return opErr
			}

			opErr = op.RunStep(op.Context(), "wait_ready", func(stepCtx context.Context) error {
				if waitErr := sdkagent.WaitReady(stepCtx, socketPath, 15*time.Second); waitErr != nil {
					return fmt.Errorf("%w (check daemon log: %s)", waitErr, sdkagent.DaemonLogPath(dataRoot))
				}
				return nil
			})
			if opErr != nil {
				return opErr
			}

			fmt.Println(ui.SuccessMsg("agent installed and running"))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("socket", socketPath),
				ui.KV("data root", dataRoot),
				ui.KV("daemon log", sdkagent.DaemonLogPath(dataRoot)),
			))
			return nil
		},
	}

	cmd.Flags().StringVar(&dataRoot, "data-root", "", "Machine data root")
	cmd.Flags().StringVar(&socketPath, "socket", "", "Daemon unix socket path")
	return cmd
}
