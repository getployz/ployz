package main

import (
	"context"
	"encoding/binary"
	"fmt"
	"net/netip"
	"os"
	"os/exec"
	"os/signal"
	"runtime"
	"strings"
	"syscall"

	"ployz/internal/machine"

	"github.com/spf13/cobra"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

type hostAccessSession interface {
	InterfaceName() string
	Close(ctx context.Context) error
}

func hostCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "host",
		Short: "Host-level access helpers",
	}
	cmd.AddCommand(hostAccessCmd())
	return cmd
}

func hostAccessCmd() *cobra.Command {
	var networkName string
	var dataRoot string
	var helperImage string

	cmd := &cobra.Command{
		Use:   "access",
		Short: "Temporary host access to overlay network",
		Long:  "Sets up temporary host WireGuard access while this command runs. Ctrl-C tears it down.",
		RunE: func(cmd *cobra.Command, args []string) error {
			if runtime.GOOS != "darwin" {
				return fmt.Errorf("host access is currently supported on macOS only")
			}
			if err := maybeReexecHostAccessWithSudo(); err != nil {
				return err
			}

			for _, tool := range []string{"docker", "sudo"} {
				if _, err := exec.LookPath(tool); err != nil {
					return fmt.Errorf("required command %q not found in PATH", tool)
				}
			}

			cfg, err := machine.NormalizeConfig(machine.Config{
				Network:     networkName,
				DataRoot:    dataRoot,
				HelperImage: helperImage,
			})
			if err != nil {
				return err
			}

			st, err := machine.LoadState(cfg)
			if err != nil {
				return err
			}
			if !st.Running {
				return fmt.Errorf("machine runtime for network %q is not running", cfg.Network)
			}
			if st.WGInterface == "" {
				return fmt.Errorf("missing wireguard interface in state")
			}
			if st.WGPort == 0 {
				st.WGPort = machine.DefaultWGPort(cfg.Network)
			}

			localSubnet, err := netip.ParsePrefix(st.Subnet)
			if err != nil {
				return fmt.Errorf("parse local subnet from state: %w", err)
			}
			hostIP, err := pickHostAccessIP(localSubnet)
			if err != nil {
				return err
			}

			networkCIDR := st.CIDR
			if strings.TrimSpace(networkCIDR) == "" {
				networkCIDR = localSubnet.String()
			}

			helperIP, err := helperIPv4(cmd.Context(), cfg.HelperName)
			if err != nil {
				return err
			}

			hostPriv, err := wgtypes.GeneratePrivateKey()
			if err != nil {
				return fmt.Errorf("generate host wireguard key: %w", err)
			}
			hostPub := hostPriv.PublicKey().String()

			peerAddScript := fmt.Sprintf(
				`set -eu; wg set %q peer %q persistent-keepalive 25 allowed-ips %q; ip route replace %q dev %q`,
				st.WGInterface,
				hostPub,
				hostIP.String()+"/32",
				hostIP.String()+"/32",
				st.WGInterface,
			)
			if err := runDockerExecScript(cmd.Context(), cfg.HelperName, peerAddScript); err != nil {
				return fmt.Errorf("configure helper peer: %w", err)
			}

			session, err := startHostAccessSession(
				cmd.Context(),
				cfg.Network,
				hostPriv.String(),
				hostIP,
				st.WGPublic,
				netip.AddrPortFrom(helperIP, uint16(st.WGPort)),
				networkCIDR,
			)
			if err != nil {
				peerRemove := fmt.Sprintf(
					`set -eu; wg set %q peer %q remove || true; ip route del %q dev %q >/dev/null 2>&1 || true`,
					st.WGInterface,
					hostPub,
					hostIP.String()+"/32",
					st.WGInterface,
				)
				_ = runDockerExecScript(context.Background(), cfg.HelperName, peerRemove)
				return fmt.Errorf("start host wireguard access: %w", err)
			}

			cleanup := func() {
				_ = session.Close(context.Background())
				peerRemove := fmt.Sprintf(
					`set -eu; wg set %q peer %q remove || true; ip route del %q dev %q >/dev/null 2>&1 || true`,
					st.WGInterface,
					hostPub,
					hostIP.String()+"/32",
					st.WGInterface,
				)
				_ = runDockerExecScript(context.Background(), cfg.HelperName, peerRemove)
			}

			fmt.Printf("host access active for network %q\n", cfg.Network)
			fmt.Printf("  interface: %s\n", session.InterfaceName())
			fmt.Printf("  host ip:   %s\n", hostIP)
			fmt.Printf("  endpoint:  %s:%d\n", helperIP, st.WGPort)
			fmt.Printf("  routes:    %s\n", networkCIDR)
			fmt.Println("Press Ctrl-C to tear down host access")

			sigCtx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()
			<-sigCtx.Done()
			cleanup()
			fmt.Println("host access stopped")
			return nil
		},
	}

	cmd.Flags().StringVar(&networkName, "network", "default", "Network identifier")
	cmd.Flags().StringVar(&dataRoot, "data-root", machine.DefaultDataRoot(), "Machine data root")
	cmd.Flags().StringVar(&helperImage, "helper-image", "", "Linux helper image for macOS")
	return cmd
}

func pickHostAccessIP(subnet netip.Prefix) (netip.Addr, error) {
	subnet = subnet.Masked()
	if !subnet.IsValid() || !subnet.Addr().Is4() {
		return netip.Addr{}, fmt.Errorf("host access requires ipv4 subnet")
	}
	if subnet.Bits() > 30 {
		return netip.Addr{}, fmt.Errorf("subnet %s is too small for host access IP", subnet)
	}
	b := subnet.Addr().As4()
	start := binary.BigEndian.Uint32(b[:])
	hostBits := uint32(32 - subnet.Bits())
	size := uint32(1) << hostBits
	if size < 4 {
		return netip.Addr{}, fmt.Errorf("subnet %s has no spare host address", subnet)
	}
	candidate := start + size - 2
	var out [4]byte
	binary.BigEndian.PutUint32(out[:], candidate)
	return netip.AddrFrom4(out), nil
}

func helperIPv4(ctx context.Context, helperName string) (netip.Addr, error) {
	out, err := runDockerExecScriptOutput(ctx, helperName, `set -eu
ip -4 -o addr show dev eth0 | awk 'NR==1 {print $4}' | cut -d/ -f1`)
	if err != nil {
		return netip.Addr{}, fmt.Errorf("read helper eth0 address: %w", err)
	}
	ip := strings.TrimSpace(out)
	addr, err := netip.ParseAddr(ip)
	if err != nil {
		return netip.Addr{}, fmt.Errorf("parse helper IP %q: %w", ip, err)
	}
	return addr, nil
}

func runDockerExecScript(ctx context.Context, containerName, script string) error {
	_, err := runDockerExecScriptOutput(ctx, containerName, script)
	return err
}

func runDockerExecScriptOutput(ctx context.Context, containerName, script string) (string, error) {
	cmd := exec.CommandContext(ctx, "docker", "exec", containerName, "sh", "-lc", script)
	out, err := cmd.CombinedOutput()
	if err != nil {
		msg := strings.TrimSpace(string(out))
		if msg == "" {
			return "", fmt.Errorf("docker exec %s failed: %w", containerName, err)
		}
		return "", fmt.Errorf("docker exec %s failed: %w: %s", containerName, err, msg)
	}
	return strings.TrimSpace(string(out)), nil
}

func runSudo(ctx context.Context, name string, args ...string) error {
	if os.Geteuid() == 0 {
		cmd := exec.CommandContext(ctx, name, args...)
		out, err := cmd.CombinedOutput()
		if err != nil {
			msg := strings.TrimSpace(string(out))
			if msg == "" {
				return fmt.Errorf("%s failed: %w", name, err)
			}
			return fmt.Errorf("%s failed: %w: %s", name, err, msg)
		}
		return nil
	}

	all := append([]string{name}, args...)
	cmd := exec.CommandContext(ctx, "sudo", append([]string{"-n"}, all...)...)
	out, err := cmd.CombinedOutput()
	if err != nil {
		msg := strings.TrimSpace(string(out))
		if msg == "" {
			return fmt.Errorf("sudo %s failed: %w (run command with sudo privileges)", name, err)
		}
		return fmt.Errorf("sudo %s failed: %w: %s", name, err, msg)
	}
	return nil
}

func maybeReexecHostAccessWithSudo() error {
	if os.Geteuid() == 0 {
		return nil
	}
	if os.Getenv("PLOYZ_HOST_ACCESS_SUDO") == "1" {
		return fmt.Errorf("host access requires sudo privileges")
	}

	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("find executable for sudo re-exec: %w", err)
	}
	sudoPath, err := exec.LookPath("sudo")
	if err != nil {
		return fmt.Errorf("sudo not found in PATH")
	}

	preserve := "DOCKER_HOST,DOCKER_CONTEXT,HOME,PATH"
	args := []string{"sudo", "--preserve-env=" + preserve, "PLOYZ_HOST_ACCESS_SUDO=1", exe}
	args = append(args, os.Args[1:]...)
	return syscall.Exec(sudoPath, args, os.Environ())
}

func hostInterfaceName(network string) string {
	n := strings.TrimSpace(network)
	if n == "" {
		n = "default"
	}
	name := "plzhost-" + n
	if len(name) > 15 {
		name = name[:15]
	}
	return name
}
