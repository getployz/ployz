package node

import (
	"fmt"
	"os"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/cluster"
	sdkmachine "ployz/pkg/sdk/machine"

	"github.com/spf13/cobra"
)

func addCmd() *cobra.Command {
	var cf cmdutil.ContextFlags
	var endpoint string
	var sshPort int
	var sshKey string
	var wgPort int

	cmd := &cobra.Command{
		Use:   "add <user@host>",
		Short: "Add a machine over SSH",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			contextName, svc, cl, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			diag, err := svc.Diagnose(cmd.Context())
			if err != nil {
				return fmt.Errorf("diagnose runtime: %w", err)
			}
			if !diag.ControlPlaneReady() {
				cmdutil.PrintStatusIssues(os.Stderr, diag.ControlPlaneBlockers, cmdutil.IssueLevelBlocker)
				return fmt.Errorf("control plane is not ready; run `ployz doctor`")
			}

			fmt.Fprintln(os.Stderr, ui.InfoMsg("adding machine %s", ui.Accent(args[0])))

			telemetryOut := ui.NewTelemetryOutput()
			defer telemetryOut.Close()

			result, err := svc.AddMachine(cmd.Context(), sdkmachine.AddOptions{
				Network:  cl.Network,
				Target:   args[0],
				Endpoint: endpoint,
				SSHPort:  sshPort,
				SSHKey:   sshKey,
				WGPort:   wgPort,
				Tracer:   telemetryOut.Tracer("ployz/sdk/machine"),
			})
			if err != nil {
				return err
			}

			// Persist the SSH connection to config.
			cfg, err := cluster.LoadDefault()
			if err != nil {
				return fmt.Errorf("load context config: %w", err)
			}
			entry, ok := cfg.Cluster(contextName)
			if !ok {
				return fmt.Errorf("context %q not found in config", contextName)
			}
			entry.Connections = append(entry.Connections, cluster.Connection{
				SSH:        args[0],
				SSHKeyFile: sshKey,
			})
			cfg.Upsert(contextName, entry)
			if err := cfg.Save(); err != nil {
				return fmt.Errorf("save context config: %w", err)
			}

			fmt.Println(ui.SuccessMsg("added machine %s via context %s", ui.Accent(args[0]), ui.Accent(contextName)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("endpoint", result.Machine.Endpoint),
				ui.KV("key", result.Machine.PublicKey),
				ui.KV("subnet", result.Machine.Subnet),
				ui.KV("peers", fmt.Sprintf("%d", result.Peers)),
			))
			return nil
		},
	}

	cf.Bind(cmd)
	cmd.Flags().StringVar(&endpoint, "endpoint", "", "Remote WireGuard endpoint to advertise (ip:port)")
	cmd.Flags().IntVar(&sshPort, "ssh-port", 22, "SSH port")
	cmd.Flags().StringVar(&sshKey, "ssh-key", "", "SSH private key path")
	cmd.Flags().IntVar(&wgPort, "wg-port", 0, "Remote WireGuard listen port (default derived from network)")
	return cmd
}
