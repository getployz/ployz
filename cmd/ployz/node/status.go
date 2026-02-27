package node

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	var cf cmdutil.ContextFlags

	cmd := &cobra.Command{
		Use:   "status",
		Short: "Show machine runtime status",
		RunE: func(cmd *cobra.Command, args []string) error {
			_, svc, _, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			diag, err := svc.Diagnose(cmd.Context())
			if err != nil {
				return err
			}
			status := diag.Status

			fmt.Print(ui.KeyValues("",
				ui.KV("network phase", fallbackDash(status.NetworkPhase)),
				ui.KV("supervisor phase", fallbackDash(status.SupervisorPhase)),
				ui.KV("clock phase", fallbackDash(status.ClockPhase)),
				ui.KV("service ready", ui.Bool(diag.ServiceReady())),
				ui.KV("control plane ready", ui.Bool(diag.ControlPlaneReady())),
				ui.KV("state", fallbackDash(status.StatePath)),
			))

			if len(diag.ControlPlaneBlockers) > 0 {
				fmt.Println(ui.ErrorMsg("blocking issues:"))
				cmdutil.PrintStatusIssues(cmd.OutOrStdout(), diag.ControlPlaneBlockers, cmdutil.IssueLevelBlocker)
			}
			if len(diag.Warnings) > 0 {
				fmt.Println(ui.WarnMsg("warnings:"))
				cmdutil.PrintStatusIssues(cmd.OutOrStdout(), diag.Warnings, cmdutil.IssueLevelWarning)
			}
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}

func fallbackDash(value string) string {
	if value == "" {
		return "-"
	}
	return value
}
