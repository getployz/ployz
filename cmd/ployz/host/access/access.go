package access

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

	"ployz/cmd/ployz/cmdutil"
	"ployz/cmd/ployz/ui"
	"ployz/pkg/sdk/client"
	sdkmachine "ployz/pkg/sdk/machine"

	"github.com/spf13/cobra"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

type session interface {
	InterfaceName() string
	Close(ctx context.Context) error
}

func Cmd() *cobra.Command {
	var nf cmdutil.NetworkFlags
	var socketPath string

	cmd := &cobra.Command{
		Use:   "access",
		Short: "Temporary host access to overlay network",
		Long:  "Sets up temporary host WireGuard access while this command runs. Ctrl-C tears it down.",
		RunE: func(cmd *cobra.Command, args []string) error {
			if runtime.GOOS != "darwin" {
				return fmt.Errorf("host access is currently supported on macOS only")
			}
			if err := maybeReexecWithSudo(); err != nil {
				return err
			}

			for _, tool := range []string{"docker", "sudo"} {
				if _, err := exec.LookPath(tool); err != nil {
					return fmt.Errorf("required command %q not found in PATH", tool)
				}
			}

			resolvedSocket, err := cmdutil.ResolveSocketPath(socketPath)
			if err != nil {
				return err
			}
			api, err := client.NewUnix(resolvedSocket)
			if err != nil {
				return fmt.Errorf("connect to daemon: %w", err)
			}
			svc := sdkmachine.New(api)

			status, err := svc.Status(cmd.Context(), nf.Network)
			if err != nil {
				return err
			}
			if !status.Running {
				return fmt.Errorf("machine runtime for network %q is not running", nf.Network)
			}

			identity, err := svc.Identity(cmd.Context(), nf.Network)
			if err != nil {
				return err
			}

			wgInterface := strings.TrimSpace(identity.WGInterface)
			if wgInterface == "" {
				return fmt.Errorf("missing wireguard interface in state")
			}
			localSubnet, err := netip.ParsePrefix(strings.TrimSpace(identity.Subnet))
			if err != nil {
				return fmt.Errorf("parse local subnet from state: %w", err)
			}
			hostIP, err := pickHostAccessIP(localSubnet)
			if err != nil {
				return err
			}

			networkCIDR := strings.TrimSpace(identity.NetworkCIDR)
			if strings.TrimSpace(networkCIDR) == "" {
				networkCIDR = localSubnet.String()
			}

			endpoint, err := svc.HostAccessEndpoint(cmd.Context(), nf.Network)
			if err != nil {
				return err
			}

			hostPriv, err := wgtypes.GeneratePrivateKey()
			if err != nil {
				return fmt.Errorf("generate host wireguard key: %w", err)
			}
			hostPub := hostPriv.PublicKey().String()

			if err := svc.AddHostAccessPeer(cmd.Context(), nf.Network, hostPub, hostIP); err != nil {
				return fmt.Errorf("configure host access peer: %w", err)
			}

			sess, err := startSession(
				cmd.Context(),
				nf.Network,
				hostPriv.String(),
				hostIP,
				identity.PublicKey,
				endpoint,
				networkCIDR,
			)
			if err != nil {
				_ = svc.RemoveHostAccessPeer(context.Background(), nf.Network, hostPub, hostIP)
				return fmt.Errorf("start host wireguard access: %w", err)
			}

			cleanup := func() {
				_ = sess.Close(context.Background())
				_ = svc.RemoveHostAccessPeer(context.Background(), nf.Network, hostPub, hostIP)
			}

			fmt.Println(ui.InfoMsg("host access active for network %s", ui.Accent(nf.Network)))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("interface", sess.InterfaceName()),
				ui.KV("host ip", hostIP.String()),
				ui.KV("endpoint", endpoint.String()),
				ui.KV("routes", networkCIDR),
			))
			fmt.Println(ui.Muted("Press Ctrl-C to tear down host access"))

			sigCtx, stop := signal.NotifyContext(cmd.Context(), os.Interrupt, syscall.SIGTERM)
			defer stop()
			<-sigCtx.Done()
			cleanup()
			fmt.Println(ui.SuccessMsg("host access stopped"))
			return nil
		},
	}

	nf.Bind(cmd)
	_ = cmd.Flags().MarkHidden("data-root")
	_ = cmd.Flags().MarkHidden("helper-image")
	cmd.Flags().StringVar(&socketPath, "socket", cmdutil.DefaultSocketPath(), "ployzd unix socket path")
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

func maybeReexecWithSudo() error {
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
