package configure

import (
	"context"
	"fmt"

	"ployz/cmd/ployz/ui"
	"ployz/internal/infra/wireguard"
	sdkconfigure "ployz/pkg/sdk/configure"
	"ployz/pkg/sdk/telemetry"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	var opts sdkconfigure.StatusOptions

	cmd := &cobra.Command{
		Use:   "status",
		Short: "Show daemon and helper status",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			svc := sdkconfigure.New()
			telemetryOut := ui.NewTelemetryOutput()
			defer telemetryOut.Close()

			op, err := telemetry.EmitPlan(cmd.Context(), telemetryOut.Tracer("ployz/cmd/configure"), "configure.status", telemetry.Plan{Steps: []telemetry.PlannedStep{{
				ID:    "status",
				Title: "checking daemon and helper status",
			}}})
			if err != nil {
				return err
			}
			var opErr error
			defer func() {
				op.End(opErr)
			}()

			var st sdkconfigure.StatusResult
			opErr = op.RunStep(op.Context(), "status", func(stepCtx context.Context) error {
				resolved, statusErr := svc.Status(stepCtx, opts)
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
				ui.KV("daemon ready", ui.Bool(st.DaemonReady)),
				ui.KV("helper installed", ui.Bool(st.HelperInstalled)),
				ui.KV("helper running", ui.Bool(st.HelperRunning)),
				ui.KV("helper socket ready", ui.Bool(st.HelperSocketReady)),
				ui.KV("helper token present", ui.Bool(st.HelperTokenPresent)),
				ui.KV("daemon socket", st.SocketPath),
				ui.KV("helper socket", st.PrivSocketPath),
				ui.KV("helper token", st.TokenPath),
				ui.KV("data root", st.DataRoot),
				ui.KV("daemon log", st.DaemonLogPath),
			))

			if st.DaemonStatusError != "" {
				fmt.Println(ui.WarnMsg("daemon status check failed: %s", st.DaemonStatusError))
			}
			if st.DaemonReadyError != "" {
				fmt.Println(ui.WarnMsg("daemon readiness check failed: %s", st.DaemonReadyError))
			}
			if st.HelperStatusError != "" {
				fmt.Println(ui.WarnMsg("helper service status check failed: %s", st.HelperStatusError))
			}

			return nil
		},
	}

	cmd.Flags().StringVar(&opts.DataRoot, "data-root", "", "Machine data root")
	cmd.Flags().StringVar(&opts.SocketPath, "socket", "", "Daemon unix socket path")
	cmd.Flags().StringVar(&opts.PrivSocketPath, "priv-socket", wireguard.DefaultPrivilegedSocketPath(), "Unix socket path used by privileged helper")
	cmd.Flags().StringVar(&opts.TokenPath, "token-path", wireguard.DefaultPrivilegedTokenPath(), "Path to privileged helper token file")

	return cmd
}
