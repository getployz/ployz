package node

import (
	"fmt"
	"strconv"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func listCmd() *cobra.Command {
	var cf cmdutil.ContextFlags

	cmd := &cobra.Command{
		Use:     "list",
		Aliases: []string{"ls"},
		Short:   "List machines in the selected context",
		RunE: func(cmd *cobra.Command, args []string) error {
			_, svc, _, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			diag, diagErr := svc.Diagnose(cmd.Context())
			if diagErr == nil && !diag.ControlPlaneReady() {
				fmt.Println(ui.WarnMsg("control plane is degraded; membership data may be stale"))
				cmdutil.PrintStatusIssues(cmd.OutOrStdout(), diag.ControlPlaneBlockers, cmdutil.IssueLevelWarning)
			}

			machines, err := svc.ListMachines(cmd.Context())
			if err != nil {
				return err
			}
			if len(machines) == 0 {
				fmt.Println(ui.Muted("no machines registered"))
				return nil
			}

			rows := make([][]string, len(machines))
			for i, m := range machines {
				updated := strings.TrimSpace(m.LastUpdated)
				if updated == "" {
					updated = "-"
				}
				version := "-"
				if m.Version > 0 {
					version = strconv.FormatInt(m.Version, 10)
				}
				lag := "-"
				if m.ReplicationLag > 0 {
					lag = fmt.Sprintf("%.0fms", float64(m.ReplicationLag.Milliseconds()))
				}
				freshness := "-"
				if m.Freshness > 0 {
					freshness = fmt.Sprintf("%.0fms", float64(m.Freshness.Milliseconds()))
					if m.Stale {
						freshness = ui.Warn(freshness)
					}
				}
				rows[i] = []string{
					strconv.Itoa(i + 1),
					m.ID,
					m.Subnet,
					m.ManagementIP,
					m.Endpoint,
					version,
					lag,
					freshness,
					updated,
				}
			}

			fmt.Println(ui.Table(
				[]string{"#", "ID", "Subnet", "Management", "Endpoint", "Version", "Lag", "Freshness", "Updated"},
				rows,
			))
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}
