package machine

import (
	"fmt"
	"net/netip"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	machinelib "ployz/internal/machine"

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

	cmd := &cobra.Command{
		Use:   "start",
		Short: "Start local machine in a network",
		RunE: func(cmd *cobra.Command, args []string) error {
			cidrPfx, err := netip.ParsePrefix(cidr)
			if err != nil {
				return fmt.Errorf("parse cidr: %w", err)
			}
			cfg := nf.Config()
			cfg.NetworkCIDR = cidrPfx
			cfg.WGPort = wgPort
			cfg.AdvertiseEP = advertiseEndpoint
			cfg.CorrosionBootstrap = bootstrap

			if subnet != "" {
				subnetPfx, sErr := netip.ParsePrefix(subnet)
				if sErr != nil {
					return fmt.Errorf("parse subnet: %w", sErr)
				}
				cfg.Subnet = subnetPfx
			}
			if mgmt != "" {
				addr, aErr := netip.ParseAddr(mgmt)
				if aErr != nil {
					return fmt.Errorf("parse management ip: %w", aErr)
				}
				cfg.Management = addr
			}

			ctrl, err := machinelib.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			out, err := ctrl.Start(cmd.Context(), cfg)
			if err != nil {
				return err
			}

			corrosion := fmt.Sprintf("%s (api %s, gossip %s)", out.CorrosionName, out.CorrosionAPIAddr, out.CorrosionGossipAP)
			fmt.Println(ui.SuccessMsg("started machine for network %s", ui.Accent(out.Network)))
			kvs := []ui.Pair{
				ui.KV("cidr", out.NetworkCIDR.String()),
				ui.KV("subnet", out.Subnet.String()),
				ui.KV("wireguard", out.WGInterface),
				ui.KV("docker", out.DockerNetwork),
				ui.KV("corrosion", corrosion),
			}
			if out.AdvertiseEP != "" {
				kvs = append(kvs, ui.KV("advertise", out.AdvertiseEP))
			}
			fmt.Print(ui.KeyValues("  ", kvs...))
			return nil
		},
	}

	nf.Bind(cmd)
	cmd.Flags().StringVar(&cidr, "cidr", machinelib.DefaultNetworkCIDR, "Network CIDR pool")
	cmd.Flags().StringVar(&subnet, "subnet", "", "Machine subnet (optional; auto-allocated if unset)")
	cmd.Flags().StringVar(&mgmt, "management-ip", "", "WireGuard management IP (default derived from machine public key)")
	cmd.Flags().StringVar(&advertiseEndpoint, "advertise-endpoint", "", "WireGuard endpoint to advertise (ip:port)")
	cmd.Flags().StringSliceVar(&bootstrap, "bootstrap", nil, "Corrosion bootstrap addresses (host:port)")
	cmd.Flags().IntVar(&wgPort, "wg-port", 0, "WireGuard listen port (default derived from network)")
	return cmd
}
