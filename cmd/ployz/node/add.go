package node

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/cluster"
	sdkmachine "ployz/pkg/sdk/machine"

	"github.com/spf13/cobra"
)

func addCmd() *cobra.Command {
	var cf cmdutil.ClusterFlags
	var endpoint string
	var sshPort int
	var sshKey string
	var wgPort int

	cmd := &cobra.Command{
		Use:   "add <user@host>",
		Short: "Add a node to the cluster over SSH",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			clusterName, svc, cl, err := service(cmd.Context(), &cf)
			if err != nil {
				return err
			}

			result, err := svc.AddMachine(cmd.Context(), sdkmachine.AddOptions{
				Network:  cl.Network,
				Target:   args[0],
				Endpoint: endpoint,
				SSHPort:  sshPort,
				SSHKey:   sshKey,
				WGPort:   wgPort,
			})
			if err != nil {
				return err
			}

			// Persist the SSH connection to config.
			cfg, _ := cluster.LoadDefault()
			entry, _ := cfg.Cluster(clusterName)
			entry.Connections = append(entry.Connections, cluster.Connection{
				SSH:        args[0],
				SSHKeyFile: sshKey,
			})
			cfg.Upsert(clusterName, entry)
			_ = cfg.Save()

			fmt.Println(ui.SuccessMsg("added node %s to cluster %s", ui.Accent(args[0]), ui.Accent(clusterName)))
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
