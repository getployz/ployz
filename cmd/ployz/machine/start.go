package machine

import (
	"fmt"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/types"

	"github.com/spf13/cobra"
)

func startCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var cidr string
	var subnet string
	var mgmt string
	var wgPort int
	var advertiseEndpoint string
	var bootstrap []string
	var socketPath string

	cmd := &cobra.Command{
		Use:   "start",
		Short: "Apply network spec through ployzd",
		RunE: func(cmd *cobra.Command, args []string) error {
			resolvedSocket, err := cmdutil.ResolveSocketPath(socketPath)
			if err != nil {
				return err
			}

			svc, err := service(resolvedSocket)
			if err != nil {
				return err
			}
			out, err := svc.Start(cmd.Context(), types.NetworkSpec{
				Network:           nf.Network,
				DataRoot:          nf.DataRoot,
				HelperImage:       nf.HelperImage,
				NetworkCIDR:       cidr,
				Subnet:            subnet,
				ManagementIP:      mgmt,
				WGPort:            wgPort,
				AdvertiseEndpoint: advertiseEndpoint,
				Bootstrap:         bootstrap,
			})
			if err != nil {
				return err
			}

			corrosion := fmt.Sprintf("%s (api %s, gossip %s)", out.CorrosionName, out.CorrosionAPIAddr, out.CorrosionGossipAP)
			fmt.Println(ui.SuccessMsg("applied network spec for %s", ui.Accent(out.Network)))
			kvs := []ui.Pair{
				ui.KV("cidr", out.NetworkCIDR),
				ui.KV("subnet", out.Subnet),
				ui.KV("wireguard", out.WGInterface),
				ui.KV("docker", out.DockerNetwork),
				ui.KV("corrosion", corrosion),
			}
			if out.AdvertiseEndpoint != "" {
				kvs = append(kvs, ui.KV("advertise", out.AdvertiseEndpoint))
			}
			fmt.Print(ui.KeyValues("  ", kvs...))

			clusterName, saveErr := cmdutil.SaveOrUpdateCurrentCluster(out.Network, nf.DataRoot, resolvedSocket)
			if saveErr != nil {
				fmt.Println(ui.WarnMsg("saved network but could not update local cluster config: %v", saveErr))
			} else {
				fmt.Println(ui.Muted("  current cluster: " + clusterName))
			}
			return nil
		},
	}

	nf.Bind(cmd)
	cmd.Flags().StringVar(&cidr, "cidr", "", "Network CIDR pool")
	cmd.Flags().StringVar(&subnet, "subnet", "", "Machine subnet (optional; auto-allocated if unset)")
	cmd.Flags().StringVar(&mgmt, "management-ip", "", "WireGuard management IP (default derived from machine public key)")
	cmd.Flags().StringVar(&advertiseEndpoint, "advertise-endpoint", "", "WireGuard endpoint to advertise (ip:port)")
	cmd.Flags().StringSliceVar(&bootstrap, "bootstrap", nil, "Corrosion bootstrap addresses (host:port)")
	cmd.Flags().IntVar(&wgPort, "wg-port", 0, "WireGuard listen port (default derived from network)")
	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
	return cmd
}
