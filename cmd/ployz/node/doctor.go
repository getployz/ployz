package node

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/types"

	"github.com/spf13/cobra"
)

func doctorCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags

	cmd := &cobra.Command{
		Use:   "doctor",
		Short: "Diagnose runtime state-machine health",
		RunE: func(cmd *cobra.Command, args []string) error {
			clusterName, svc, _, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			diag, err := svc.Diagnose(cmd.Context())
			if err != nil {
				return err
			}
			status := diag.Status

			fmt.Println(ui.InfoMsg("cluster %s diagnostic", ui.Accent(clusterName)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("network phase", status.NetworkPhase),
				ui.KV("supervisor phase", status.SupervisorPhase),
				ui.KV("clock phase", status.ClockPhase),
				ui.KV("service ready", ui.Bool(diag.ServiceReady())),
				ui.KV("control plane ready", ui.Bool(diag.ControlPlaneReady())),
				ui.KV("state", status.StatePath),
			))

			if tree := status.RuntimeTree; strings.TrimSpace(tree.Component) != "" {
				fmt.Println(ui.InfoMsg("state tree"))
				printStateNode("  ", tree)
			}

			if len(diag.ControlPlaneBlockers) == 0 && len(diag.Warnings) == 0 {
				fmt.Println(ui.SuccessMsg("no issues detected"))
				return nil
			}

			if len(diag.ControlPlaneBlockers) > 0 {
				fmt.Println(ui.ErrorMsg("blocking issues:"))
				cmdutil.PrintStatusIssues(cmd.OutOrStdout(), diag.ControlPlaneBlockers, cmdutil.IssueLevelBlocker)
			}
			if len(diag.Warnings) > 0 {
				fmt.Println(ui.WarnMsg("warnings:"))
				cmdutil.PrintStatusIssues(cmd.OutOrStdout(), diag.Warnings, cmdutil.IssueLevelWarning)
			}
			if len(diag.ControlPlaneBlockers) > 0 {
				return fmt.Errorf("%d blocking issues detected", len(diag.ControlPlaneBlockers))
			}
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}

func printStateNode(prefix string, node types.StateNode) {
	health := ui.Bool(node.Healthy)
	if !node.Required {
		health = health + " (optional)"
	}
	line := fmt.Sprintf("%s- %s phase=%s healthy=%s", prefix, node.Component, node.Phase, health)
	if strings.TrimSpace(node.LastError) != "" {
		line += " error=" + node.LastError
	}
	fmt.Println(line)
	for _, child := range node.Children {
		printStateNode(prefix+"  ", child)
	}
}
