package network

import (
	"fmt"
	"os"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	sdkmachine "ployz/pkg/sdk/machine"

	"github.com/spf13/cobra"
)

func statusCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags

	cmd := &cobra.Command{
		Use:   "status [network]",
		Short: "Show network readiness for the selected context",
		Args:  cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			clusterName, api, cl, err := cf.DialService(cmd.Context())
			if err != nil {
				return err
			}
			defer func() {
				_ = api.Close()
			}()

			expectedNetwork := ""
			if len(args) == 1 {
				expectedNetwork = strings.TrimSpace(args[0])
			}
			if expectedNetwork != "" && strings.TrimSpace(cl.Network) != expectedNetwork {
				return fmt.Errorf("context %q targets network %q, not %q", clusterName, strings.TrimSpace(cl.Network), expectedNetwork)
			}

			svc := sdkmachine.New(api)
			diag, err := svc.Diagnose(cmd.Context())
			if err != nil {
				return err
			}

			fmt.Print(ui.KeyValues("",
				ui.KV("context", clusterName),
				ui.KV("network", cl.Network),
				ui.KV("network phase", fallbackDash(diag.Status.NetworkPhase)),
				ui.KV("service ready", ui.Bool(diag.ServiceReady())),
				ui.KV("control plane ready", ui.Bool(diag.ControlPlaneReady())),
			))

			if len(diag.ControlPlaneBlockers) > 0 {
				fmt.Println(ui.ErrorMsg("blocking issues:"))
				cmdutil.PrintStatusIssues(os.Stdout, diag.ControlPlaneBlockers, cmdutil.IssueLevelBlocker)
			}
			if len(diag.Warnings) > 0 {
				fmt.Println(ui.WarnMsg("warnings:"))
				cmdutil.PrintStatusIssues(os.Stdout, diag.Warnings, cmdutil.IssueLevelWarning)
			}

			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}

func fallbackDash(value string) string {
	if strings.TrimSpace(value) == "" {
		return "-"
	}
	return value
}
