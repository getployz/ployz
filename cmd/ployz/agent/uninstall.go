package agent

import (
	"context"
	"fmt"

	"ployz/cmd/ployz/ui"
	sdkagent "ployz/pkg/sdk/agent"
	"ployz/pkg/sdk/telemetry"

	"github.com/spf13/cobra"
)

func uninstallCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "uninstall",
		Short: "Stop and remove the ployz agent services",
		RunE: func(cmd *cobra.Command, args []string) error {
			svc := sdkagent.NewPlatformService()
			telemetryOut := ui.NewTelemetryOutput()
			defer telemetryOut.Close()

			op, err := telemetry.EmitPlan(cmd.Context(), telemetryOut.Tracer("ployz/cmd/agent"), "agent.uninstall", telemetry.Plan{Steps: []telemetry.PlannedStep{{
				ID:    "uninstall",
				Title: "removing agent services",
			}}})
			if err != nil {
				return err
			}
			var opErr error
			defer func() {
				op.End(opErr)
			}()

			opErr = op.RunStep(op.Context(), "uninstall", func(stepCtx context.Context) error {
				if uninstallErr := svc.Uninstall(stepCtx); uninstallErr != nil {
					return fmt.Errorf("uninstall agent: %w", uninstallErr)
				}
				return nil
			})
			if opErr != nil {
				return opErr
			}

			fmt.Println(ui.SuccessMsg("agent services removed"))
			return nil
		},
	}
}
