package machine

import (
	"context"
	"fmt"
	"net"
	"net/netip"
	"runtime"
	"strings"
	"time"

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	machinelib "ployz/internal/machine"
	machineremote "ployz/internal/machine/remote"

	"github.com/spf13/cobra"
)

func addCmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
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
				wgPort = machinelib.DefaultWGPort(nf.Network)
			}
			remoteEP, err := resolveAdvertiseEndpoint(target, endpoint, wgPort)
			if err != nil {
				return err
			}

			cfg := nf.Config()
			ctrl, err := machinelib.New()
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

			if err := machineremote.RunScript(cmd.Context(), target, sshOpts, machineremote.StartScript(nf.Network, plan, remoteEP, wgPort)); err != nil {
				return err
			}

			remoteWGPublic, err := machineremote.FetchWGPublicKey(cmd.Context(), target, sshOpts, nf.Network)
			if err != nil {
				return err
			}

			remoteMgmtIP, err := machinelib.ManagementIPFromPublicKey(remoteWGPublic)
			if err != nil {
				return fmt.Errorf("derive remote management IP: %w", err)
			}
			if err := ctrl.UpsertMachine(cmd.Context(), cfg, machinelib.Machine{
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
				machineremote.WireGuardBootstrapScript(nf.Network, plan.LocalWGKey, plan.LocalSubnet, plan.LocalMgmtIP),
			); err != nil {
				return err
			}

			helperName := "ployz-helper-" + nf.Network
			if err := kickWireGuardPath(cmd.Context(), helperName, remoteMgmtIP); err != nil {
				fmt.Println(ui.WarnMsg("failed to kick wireguard path to %s: %v", remoteMgmtIP, err))
			}

			if err := machineremote.RunScript(cmd.Context(), target, sshOpts, machineremote.ReconcileRetryScript(nf.Network)); err != nil {
				return err
			}

			count, err := ctrl.Reconcile(cmd.Context(), cfg)
			if err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("added machine %s to network %s", ui.Accent(target), ui.Accent(nf.Network)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("endpoint", remoteEP),
				ui.KV("key", remoteWGPublic),
				ui.KV("subnet", plan.Subnet.String()),
				ui.KV("peers", fmt.Sprintf("%d", count)),
			))
			return nil
		},
	}

	nf.Bind(cmd)
	cmd.Flags().StringVar(&endpoint, "endpoint", "", "Remote WireGuard endpoint to advertise (ip:port)")
	cmd.Flags().IntVar(&sshPort, "ssh-port", 22, "SSH port")
	cmd.Flags().StringVar(&sshKey, "ssh-key", "", "SSH private key path")
	cmd.Flags().IntVar(&wgPort, "wg-port", 0, "Remote WireGuard listen port (default derived from network)")
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

func kickWireGuardPath(ctx context.Context, helperName string, remoteMgmtIP netip.Addr) error {
	if !remoteMgmtIP.IsValid() {
		return fmt.Errorf("remote management IP is required")
	}

	if runtime.GOOS == "darwin" {
		targetIP := remoteMgmtIP.String()
		isV6 := 0
		if remoteMgmtIP.Is6() {
			isV6 = 1
		}
		script := fmt.Sprintf(`set -eu
if ! ip route get 8.8.8.8 >/dev/null 2>&1; then
  cidr=$(ip -4 -o addr show dev eth0 | awk 'NR==1 {print $4}')
  if [ -n "$cidr" ]; then
    gw=$(printf '%%s\n' "$cidr" | awk -F'[./]' '{print $1"."$2"."$3".1"}')
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
fi`, cmdutil.ShellQuote(targetIP), isV6)

		return cmdutil.RunDockerExecScript(ctx, helperName, script)
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
