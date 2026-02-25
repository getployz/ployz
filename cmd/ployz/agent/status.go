package agent

import (
	"context"
	"fmt"

	"ployz/cmd/ployz/ui"
	sdkagent "ployz/pkg/sdk/agent"
	"ployz/pkg/sdk/telemetry"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show agent service health",
		RunE: func(cmd *cobra.Command, args []string) error {
			svc := sdkagent.NewPlatformService()
			telemetryOut := ui.NewTelemetryOutput()
			defer telemetryOut.Close()

			op, err := telemetry.EmitPlan(cmd.Context(), telemetryOut.Tracer("ployz/cmd/agent"), "agent.status", telemetry.Plan{Steps: []telemetry.PlannedStep{{
				ID:    "status",
				Title: "checking agent status",
			}}})
			if err != nil {
				return err
			}
			var opErr error
			defer func() {
				op.End(opErr)
			}()

			var st sdkagent.ServiceStatus
			opErr = op.RunStep(op.Context(), "status", func(stepCtx context.Context) error {
				resolved, statusErr := svc.Status(stepCtx)
				if statusErr != nil {
					return statusErr
				}
				st = resolved
				return nil
			})
			if opErr != nil {
				return opErr
			}

			fmt.Print(ui.KeyValues("",
				ui.KV("platform", st.Platform),
				ui.KV("daemon installed", ui.Bool(st.DaemonInstalled)),
				ui.KV("daemon running", ui.Bool(st.DaemonRunning)),
			))
			return nil
		},
	}
}
