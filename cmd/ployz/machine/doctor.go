package machine

import (
	"fmt"
	"strings"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"

	"github.com/spf13/cobra"
)

func doctorCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var socketPath string

	cmd := &cobra.Command{
		Use:   "doctor",
		Short: "Diagnose per-component health from ployzd",
		RunE: func(cmd *cobra.Command, args []string) error {
			svc, err := service(socketPath)
			if err != nil {
				return err
			}
			socketArg := ""
			if strings.TrimSpace(socketPath) != "" {
				socketArg = " --socket " + socketPath
			}
			startCmd := "ployz machine start --network " + nf.Network + socketArg
			reconcileCmd := "ployz machine reconcile --network " + nf.Network + socketArg
			daemonStartCmd := "ployz daemon start" + socketArg

			status, err := svc.Status(cmd.Context(), nf.Network)
			if err != nil {
				return err
			}

			identity, _ := svc.Identity(cmd.Context(), nf.Network)

			fmt.Println(ui.InfoMsg("network %s diagnostic", ui.Accent(nf.Network)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("configured", ui.Bool(status.Configured)),
				ui.KV("running", ui.Bool(status.Running)),
				ui.KV("wireguard", ui.Bool(status.WireGuard)),
				ui.KV("corrosion", ui.Bool(status.Corrosion)),
				ui.KV("docker", ui.Bool(status.DockerNet)),
				ui.KV("worker", ui.Bool(status.WorkerRunning)),
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
					fix:       startCmd,
				})
			}
			if status.Configured && !status.Running {
				issues = append(issues, issue{
					component: "runtime",
					problem:   "network state exists but runtime is stopped",
					fix:       startCmd,
				})
			}
			if !status.WireGuard {
				problem := "wireguard interface is missing or down"
				if strings.TrimSpace(identity.WGInterface) != "" {
					problem = "wireguard interface " + identity.WGInterface + " is missing or down"
				}
				issues = append(issues, issue{
					component: "wireguard",
					problem:   problem,
					fix:       startCmd,
				})
			}
			if !status.Corrosion {
				issues = append(issues, issue{
					component: "corrosion",
					problem:   "corrosion container is not healthy",
					fix:       startCmd,
				})
			}
			if !status.DockerNet {
				issues = append(issues, issue{
					component: "docker",
					problem:   "overlay docker network is missing",
					fix:       startCmd,
				})
			}
			if !status.WorkerRunning {
				issues = append(issues, issue{
					component: "daemon",
					problem:   "reconcile worker is not running",
					fix:       daemonStartCmd + " && " + reconcileCmd,
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

	nf.Bind(cmd)
	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	return cmd
}
