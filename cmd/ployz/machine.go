package main

import (
	"fmt"
	"net/netip"
	"time"

	"ployz/internal/machine"

	"github.com/spf13/cobra"
)

func machineCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "machine",
		Short: "Manage machine runtime and membership",
	}
	cmd.AddCommand(machineStartCmd())
	cmd.AddCommand(machineStopCmd())
	cmd.AddCommand(machineStatusCmd())
	cmd.AddCommand(machineAddCmd())
	cmd.AddCommand(machineListCmd())
	cmd.AddCommand(machineRemoveCmd())
	cmd.AddCommand(machineReconcileCmd())
	cmd.AddCommand(machineWatchCmd())
	return cmd
}

func controllerCmd() *cobra.Command {
	cmd := machineCmd()
	cmd.Use = "controller"
	cmd.Short = "Deprecated alias for machine"
	cmd.Hidden = true
	return cmd
}

func machineStartCmd() *cobra.Command {
	var networkName string
	var dataRoot string
	var cidr string
	var subnet string
	var mgmt string
	var wgPort int
	var helperImage string
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
			cfg := machine.Config{
				Network:            networkName,
				DataRoot:           dataRoot,
				NetworkCIDR:        cidrPfx,
				WGPort:             wgPort,
				HelperImage:        helperImage,
				AdvertiseEP:        advertiseEndpoint,
				CorrosionBootstrap: bootstrap,
			}
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

			ctrl, err := machine.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			out, err := ctrl.Start(cmd.Context(), cfg)
			if err != nil {
				return err
			}
			fmt.Printf("started machine for network %q\n", out.Network)
			fmt.Printf("  cidr:      %s\n", out.NetworkCIDR)
			fmt.Printf("  subnet:    %s\n", out.Subnet)
			fmt.Printf("  wireguard: %s\n", out.WGInterface)
			fmt.Printf("  docker:    %s\n", out.DockerNetwork)
			fmt.Printf("  corrosion: %s (api %s, gossip %s)\n", out.CorrosionName, out.CorrosionAPIAddr, out.CorrosionGossipAP)
			if out.AdvertiseEP != "" {
				fmt.Printf("  advertise: %s\n", out.AdvertiseEP)
			}
			return nil
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&cidr, "cidr", machine.DefaultNetworkCIDR, "Network CIDR pool")
	cmd.Flags().StringVar(&subnet, "subnet", "", "Machine subnet (optional; auto-allocated if unset)")
	cmd.Flags().StringVar(&mgmt, "management-ip", "", "WireGuard management IP (default derived from machine public key)")
	cmd.Flags().StringVar(&advertiseEndpoint, "advertise-endpoint", "", "WireGuard endpoint to advertise (ip:port)")
	cmd.Flags().StringSliceVar(&bootstrap, "bootstrap", nil, "Corrosion bootstrap addresses (host:port)")
	cmd.Flags().IntVar(&wgPort, "wg-port", 0, "WireGuard listen port (default derived from network)")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	return cmd
}

func machineStopCmd() *cobra.Command {
	var networkName string
	var dataRoot string
	var purge bool
	var helperImage string

	cmd := &cobra.Command{
		Use:   "stop",
		Short: "Stop local machine runtime for a network",
		RunE: func(cmd *cobra.Command, args []string) error {
			cfg := machine.Config{
				Network:     networkName,
				DataRoot:    dataRoot,
				HelperImage: helperImage,
			}

			ctrl, err := machine.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			out, err := ctrl.Stop(cmd.Context(), cfg, purge)
			if err != nil {
				return err
			}
			fmt.Printf("stopped machine for network %q\n", out.Network)
			if purge {
				fmt.Println("  state purged")
			}
			return nil
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().BoolVar(&purge, "purge", false, "Remove network state directory after stop")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	return cmd
}

func machineStatusCmd() *cobra.Command {
	var networkName string
	var dataRoot string
	var helperImage string

	cmd := &cobra.Command{
		Use:   "status",
		Short: "Show local machine runtime status",
		RunE: func(cmd *cobra.Command, args []string) error {
			cfg := machine.Config{
				Network:     networkName,
				DataRoot:    dataRoot,
				HelperImage: helperImage,
			}

			ctrl, err := machine.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			status, err := ctrl.Status(cmd.Context(), cfg)
			if err != nil {
				return err
			}
			fmt.Printf("configured: %t\n", status.Configured)
			fmt.Printf("running:    %t\n", status.Running)
			fmt.Printf("wireguard:  %t\n", status.WireGuard)
			fmt.Printf("corrosion:  %t\n", status.Corrosion)
			fmt.Printf("docker:     %t\n", status.DockerNet)
			fmt.Printf("state:      %s\n", status.StatePath)
			return nil
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	return cmd
}

func machineReconcileCmd() *cobra.Command {
	var networkName, dataRoot, helperImage string

	cmd := &cobra.Command{
		Use:   "reconcile",
		Short: "Reconcile WireGuard peers from Corrosion machines table",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctrl, err := machine.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			cfg := machine.Config{
				Network:     networkName,
				DataRoot:    dataRoot,
				HelperImage: helperImage,
			}
			count, err := ctrl.Reconcile(cmd.Context(), cfg)
			if err != nil {
				return err
			}
			fmt.Printf("reconciled %d peers for network %q\n", count, networkName)
			return nil
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	return cmd
}

func machineWatchCmd() *cobra.Command {
	var networkName, dataRoot, helperImage string
	var interval time.Duration

	cmd := &cobra.Command{
		Use:   "watch",
		Short: "Continuously reconcile peers from Corrosion",
		RunE: func(cmd *cobra.Command, args []string) error {
			ctrl, err := machine.New()
			if err != nil {
				return err
			}
			defer ctrl.Close()

			cfg := machine.Config{
				Network:     networkName,
				DataRoot:    dataRoot,
				HelperImage: helperImage,
			}

			ticker := time.NewTicker(interval)
			defer ticker.Stop()

			fmt.Printf("watching network %q (interval %s)\n", networkName, interval)
			for {
				count, rErr := ctrl.Reconcile(cmd.Context(), cfg)
				if rErr != nil {
					fmt.Printf("reconcile error: %v\n", rErr)
				} else {
					fmt.Printf("reconciled %d peers\n", count)
				}

				select {
				case <-cmd.Context().Done():
					return cmd.Context().Err()
				case <-ticker.C:
				}
			}
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	cmd.Flags().DurationVar(&interval, "interval", 3*time.Second, "Reconcile interval")
	return cmd
}
