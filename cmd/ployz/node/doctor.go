package node

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func doctorCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags

	cmd := &cobra.Command{
		Use:   "doctor",
		Short: "Diagnose per-component health",
		RunE: func(cmd *cobra.Command, args []string) error {
			clusterName, svc, cl, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			initCmd := "ployz init " + clusterName

			status, err := svc.Status(cmd.Context(), cl.Network)
			if err != nil {
				return err
			}

			identity, _ := svc.Identity(cmd.Context(), cl.Network)

			fmt.Println(ui.InfoMsg("cluster %s diagnostic", ui.Accent(clusterName)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("configured", ui.Bool(status.Configured)),
				ui.KV("running", ui.Bool(status.Running)),
				ui.KV("wireguard", ui.Bool(status.WireGuard)),
				ui.KV("corrosion", ui.Bool(status.Corrosion)),
				ui.KV("docker", ui.Bool(status.DockerNet)),
				ui.KV("runtime", ui.Bool(status.WorkerRunning)),
			))

			if status.Configured && status.Running && status.WireGuard && status.Corrosion && status.DockerNet {
				fmt.Println(ui.SuccessMsg("no issues detected"))
				return nil
			}

			type issue struct {
				component string
				problem   string
				fix       string
			}
			issues := make([]issue, 0, 6)

			if !status.Configured {
				issues = append(issues, issue{
					component: "config",
					problem:   "network spec is not applied on this machine",
					fix:       initCmd,
				})
			}
			if status.Configured && !status.Running {
				issues = append(issues, issue{
					component: "runtime",
					problem:   "network state exists but runtime is stopped",
					fix:       initCmd,
				})
			}
			if !status.WireGuard {
				problem := "wireguard interface is missing or down"
				if identity.WGInterface != "" {
					problem = "wireguard interface " + identity.WGInterface + " is missing or down"
				}
				issues = append(issues, issue{
					component: "wireguard",
					problem:   problem,
					fix:       initCmd,
				})
			}
			if !status.Corrosion {
				issues = append(issues, issue{
					component: "corrosion",
					problem:   "corrosion container is not healthy",
					fix:       initCmd,
				})
			}
			if !status.DockerNet {
				issues = append(issues, issue{
					component: "docker",
					problem:   "overlay docker network is missing",
					fix:       initCmd,
				})
			}
			if !status.WorkerRunning {
				issues = append(issues, issue{
					component: "runtime",
					problem:   "runtime reconcile loop is not running",
					fix:       "ployz agent install",
				})
			}

			if len(issues) == 0 {
				fmt.Println(ui.SuccessMsg("no actionable issues detected"))
				return nil
			}

			fmt.Println(ui.WarnMsg("detected issues:"))
			for i, issue := range issues {
				fmt.Printf("  %d) %s: %s\n", i+1, issue.component, issue.problem)
				fmt.Println(ui.Muted("     fix: " + issue.fix))
			}
			return nil
		},
	}

	cf.Bind(cmd)
	return cmd
}
