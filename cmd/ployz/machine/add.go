package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/defaults"
	sdkmachine "ployz/pkg/sdk/machine"

	"github.com/spf13/cobra"
)

func addCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var endpoint string
	var sshPort int
	var sshKey string
	var wgPort int
	var socketPath string

	cmd := &cobra.Command{
		Use:   "add <user@host>",
		Short: "Add a machine through remote ployzd over SSH",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			svc, err := service(socketPath)
			if err != nil {
				return err
			}
			result, err := svc.AddMachine(cmd.Context(), sdkmachine.AddOptions{
				Network:  nf.Network,
				DataRoot: nf.DataRoot,
				Target:   args[0],
				Endpoint: endpoint,
				SSHPort:  sshPort,
				SSHKey:   sshKey,
				WGPort:   wgPort,
			})
			if err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("added machine %s to network %s", ui.Accent(args[0]), ui.Accent(nf.Network)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("endpoint", result.Machine.Endpoint),
				ui.KV("key", result.Machine.PublicKey),
				ui.KV("subnet", result.Machine.Subnet),
				ui.KV("peers", fmt.Sprintf("%d", result.Peers)),
			))
			return nil
		},
	}

	nf.Bind(cmd)
	if dataRootFlag := cmd.Flags().Lookup("data-root"); dataRootFlag != nil {
		if nf.DataRoot == defaults.DataRoot() {
			nf.DataRoot = "/var/lib/ployz/networks"
		}
		dataRootFlag.DefValue = "/var/lib/ployz/networks"
		dataRootFlag.Usage = "Remote machine data root for ployzd bootstrap"
	}
	_ = cmd.Flags().MarkHidden("helper-image")
	cmd.Flags().StringVar(&endpoint, "endpoint", "", "Remote WireGuard endpoint to advertise (ip:port)")
	cmd.Flags().IntVar(&sshPort, "ssh-port", 22, "SSH port")
	cmd.Flags().StringVar(&sshKey, "ssh-key", "", "SSH private key path")
	cmd.Flags().IntVar(&wgPort, "wg-port", 0, "Remote WireGuard listen port (default derived from network)")
	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	return cmd
}
