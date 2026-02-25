package configure

import (
	"fmt"
	"strconv"

	"ployz/cmd/ployz/ui"
	"ployz/internal/adapter/wireguard"
	sdkconfigure "ployz/pkg/sdk/configure"

	"github.com/spf13/cobra"
)

func Cmd() *cobra.Command {
	var opts sdkconfigure.Options

	cmd := &cobra.Command{
		Use:   "configure",
		Short: "Install and start privileged helper service",
		Long:  "Installs privileged helper and daemon services. This is a one-time root setup command.",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			svc := sdkconfigure.New()
			telemetryOut := ui.NewTelemetryOutput()
			defer telemetryOut.Close()
			opts.Tracer = telemetryOut.Tracer("ployz/sdk/configure")
			result, err := svc.Configure(cmd.Context(), opts)
			if err != nil {
				return err
			}

			fmt.Println(ui.SuccessMsg("configured"))
			fmt.Print(ui.KeyValues("  ",
				ui.KV("socket", result.SocketPath),
				ui.KV("data root", result.DataRoot),
				ui.KV("daemon log", result.DaemonLogPath),
				ui.KV("helper socket", result.PrivSocketPath),
				ui.KV("helper token", result.TokenPath),
				ui.KV("tun socket", result.TUNSocketPath),
				ui.KV("mtu", strconv.Itoa(result.MTU)),
			))
			return nil
		},
	}

	cmd.Flags().StringVar(&opts.DataRoot, "data-root", "", "Machine data root")
	cmd.Flags().StringVar(&opts.SocketPath, "socket", "", "Daemon unix socket path")
	cmd.Flags().StringVar(&opts.TUNSocketPath, "tun-socket", wireguard.DefaultTUNSocketPath(), "Unix socket path used for TUN fd passing")
	cmd.Flags().StringVar(&opts.PrivSocketPath, "priv-socket", wireguard.DefaultPrivilegedSocketPath(), "Unix socket path used by privileged helper")
	cmd.Flags().StringVar(&opts.TokenPath, "token-path", wireguard.DefaultPrivilegedTokenPath(), "Path to privileged helper token file")
	cmd.Flags().IntVar(&opts.MTU, "mtu", 1280, "TUN MTU to request")
	cmd.AddCommand(statusCmd())

	return cmd
}
