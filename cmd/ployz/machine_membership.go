package main

import (
	"context"
	"fmt"
	"net"
	"net/netip"
	"os/exec"
	"runtime"
	"strings"
	"time"

	"ployz/internal/machine"
	machineremote "ployz/internal/machine/remote"

	"github.com/spf13/cobra"
)

func machineAddCmd() *cobra.Command {
	var networkName, dataRoot, helperImage string
	var endpoint string
	var sshPort int
	var sshKey string
	var wgPort int

	cmd := &cobra.Command{
		Use:   "add <user@host>",
		Short: "Add a machine to this network",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			target := strings.TrimSpace(args[0])
			if target == "" {
				return fmt.Errorf("target is required")
			}
			if wgPort == 0 {
				wgPort = machine.DefaultWGPort(networkName)
			}
			remoteEP, err := resolveAdvertiseEndpoint(target, endpoint, wgPort)
			if err != nil {
				return err
			}

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

			plan, err := ctrl.PlanJoin(cmd.Context(), cfg, remoteEP)
			if err != nil {
				return err
			}

			sshOpts := machineremote.SSHOptions{Port: sshPort, KeyPath: sshKey}

			if err := machineremote.RunScript(cmd.Context(), target, sshOpts, machineremote.PreflightScript()); err != nil {
				return err
			}

			if err := machineremote.RunScript(cmd.Context(), target, sshOpts, machineremote.StartScript(networkName, plan, remoteEP, wgPort)); err != nil {
				return err
			}

			remoteWGPublic, err := machineremote.FetchWGPublicKey(cmd.Context(), target, sshOpts, networkName)
			if err != nil {
				return err
			}

			remoteMgmtIP, err := machine.ManagementIPFromPublicKey(remoteWGPublic)
			if err != nil {
				return fmt.Errorf("derive remote management IP: %w", err)
			}
			if err := ctrl.UpsertMachine(cmd.Context(), cfg, machine.Machine{
				ID:         remoteWGPublic,
				PublicKey:  remoteWGPublic,
				Subnet:     plan.Subnet.String(),
				Management: remoteMgmtIP.String(),
				Endpoint:   remoteEP,
			}); err != nil {
				return err
			}

			if _, err := ctrl.Reconcile(cmd.Context(), cfg); err != nil {
				return err
			}

			if err := machineremote.RunScript(
				cmd.Context(),
				target,
				sshOpts,
				machineremote.WireGuardBootstrapScript(networkName, plan.LocalWGKey, plan.LocalSubnet, plan.LocalMgmtIP),
			); err != nil {
				return err
			}

			if err := kickWireGuardPath(cmd.Context(), networkName, remoteMgmtIP); err != nil {
				fmt.Printf("warning: failed to kick wireguard path to %s: %v\n", remoteMgmtIP, err)
			}

			if err := machineremote.RunScript(cmd.Context(), target, sshOpts, machineremote.ReconcileRetryScript(networkName)); err != nil {
				return err
			}

			count, err := ctrl.Reconcile(cmd.Context(), cfg)
			if err != nil {
				return err
			}

			fmt.Printf("added machine %q to network %q\n", target, networkName)
			fmt.Printf("  endpoint: %s\n", remoteEP)
			fmt.Printf("  key:      %s\n", remoteWGPublic)
			fmt.Printf("  subnet:   %s\n", plan.Subnet)
			fmt.Printf("  peers:    %d\n", count)
			return nil
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	cmd.Flags().StringVar(&endpoint, "endpoint", "", "Remote WireGuard endpoint to advertise (ip:port)")
	cmd.Flags().IntVar(&sshPort, "ssh-port", 22, "SSH port")
	cmd.Flags().StringVar(&sshKey, "ssh-key", "", "SSH private key path")
	cmd.Flags().IntVar(&wgPort, "wg-port", 0, "Remote WireGuard listen port (default derived from network)")
	return cmd
}

func machineListCmd() *cobra.Command {
	var networkName, dataRoot, helperImage string

	cmd := &cobra.Command{
		Use:     "list",
		Aliases: []string{"ls"},
		Short:   "List machines in the network",
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

			machines, err := ctrl.ListMachines(cmd.Context(), cfg)
			if err != nil {
				return err
			}
			if len(machines) == 0 {
				fmt.Println("no machines registered")
				return nil
			}
			for i, m := range machines {
				lastUpdated := strings.TrimSpace(m.LastUpdated)
				if lastUpdated == "" {
					lastUpdated = "-"
				}
				fmt.Printf("%d) %s\n", i+1, m.ID)
				fmt.Printf("   subnet:     %s\n", m.Subnet)
				fmt.Printf("   management: %s\n", m.Management)
				fmt.Printf("   endpoint:   %s\n", m.Endpoint)
				fmt.Printf("   updated:    %s\n", lastUpdated)
			}
			return nil
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	return cmd
}

func machineRemoveCmd() *cobra.Command {
	var networkName, dataRoot, helperImage string

	cmd := &cobra.Command{
		Use:     "remove <machine-id-or-endpoint>",
		Aliases: []string{"rm"},
		Short:   "Remove a machine from the network",
		Args:    cobra.ExactArgs(1),
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

			if err := ctrl.RemoveMachine(cmd.Context(), cfg, args[0]); err != nil {
				return err
			}
			fmt.Printf("removed machine %q from network %q\n", args[0], networkName)
			return nil
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	return cmd
}

func resolveAdvertiseEndpoint(target, override string, wgPort int) (string, error) {
	override = strings.TrimSpace(override)
	if override != "" {
		if _, err := netip.ParseAddrPort(override); err != nil {
			return "", fmt.Errorf("parse endpoint: %w", err)
		}
		return override, nil
	}

	host := target
	if strings.Contains(target, "@") {
		parts := strings.SplitN(target, "@", 2)
		host = parts[1]
	}
	host = strings.TrimSpace(host)
	addr, err := netip.ParseAddr(host)
	if err != nil {
		return "", fmt.Errorf("target host %q is not an IP address; use --endpoint ip:port", host)
	}
	return netip.AddrPortFrom(addr, uint16(wgPort)).String(), nil
}

func kickWireGuardPath(ctx context.Context, network string, remoteMgmtIP netip.Addr) error {
	if !remoteMgmtIP.IsValid() {
		return fmt.Errorf("remote management IP is required")
	}

	if runtime.GOOS == "darwin" {
		helperName := machineHelperName(network)
		targetIP := remoteMgmtIP.String()
		isV6 := 0
		if remoteMgmtIP.Is6() {
			isV6 = 1
		}
		script := fmt.Sprintf(`set -eu
if ! ip route get 8.8.8.8 >/dev/null 2>&1; then
  cidr=$(ip -4 -o addr show dev eth0 | awk 'NR==1 {print $4}')
  if [ -n "$cidr" ]; then
    gw=$(printf '%%s\n' "$cidr" | awk -F'[./]' '{print $1"."$2"."$3".1}')
    if [ -n "$gw" ]; then
      ip route replace default via "$gw" dev eth0 >/dev/null 2>&1 || true
    fi
  fi
fi
target=%s
is_v6=%d
if command -v nc >/dev/null 2>&1; then
  if [ "$is_v6" -eq 1 ]; then
    printf 'x' | nc -6 -u -w 1 "$target" 9 || true
  else
    printf 'x' | nc -u -w 1 "$target" 9 || true
  fi
elif command -v ping >/dev/null 2>&1; then
  if [ "$is_v6" -eq 1 ]; then
    ping -6 -c 1 -W 1 "$target" >/dev/null 2>&1 || ping6 -c 1 -W 1 "$target" >/dev/null 2>&1 || true
  else
    ping -c 1 -W 1 "$target" >/dev/null 2>&1 || true
  fi
elif command -v ping6 >/dev/null 2>&1; then
  ping6 -c 1 -W 1 "$target" >/dev/null 2>&1 || true
fi`, shellQuote(targetIP), isV6)

		cmd := exec.CommandContext(ctx, "docker", "exec", helperName, "sh", "-lc", script)
		out, err := cmd.CombinedOutput()
		if err != nil {
			msg := strings.TrimSpace(string(out))
			if msg == "" {
				return fmt.Errorf("kick wireguard path via helper %q: %w", helperName, err)
			}
			return fmt.Errorf("kick wireguard path via helper %q: %w: %s", helperName, err, msg)
		}
		return nil
	}

	ctx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()

	addr := net.JoinHostPort(remoteMgmtIP.String(), "9")
	conn, err := (&net.Dialer{}).DialContext(ctx, "udp", addr)
	if err != nil {
		return fmt.Errorf("kick wireguard path to %s: %w", remoteMgmtIP, err)
	}
	defer conn.Close()

	if _, err := conn.Write([]byte{0x01}); err != nil {
		return fmt.Errorf("send wireguard kick packet to %s: %w", remoteMgmtIP, err)
	}
	return nil
}

func machineHelperName(network string) string {
	n := strings.TrimSpace(network)
	if n == "" {
		n = "default"
	}
	return "ployz-helper-" + n
}

func shellQuote(s string) string {
	return "'" + strings.ReplaceAll(s, "'", "'\"'\"'") + "'"
}
