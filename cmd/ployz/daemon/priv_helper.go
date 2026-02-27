package daemon

import (
	"fmt"
	"os"
	"strings"

	"ployz/internal/infra/wireguard"

	"github.com/spf13/cobra"
)

const defaultHelperMTU = 1280

var (
	runPrivilegedHelper = wireguard.RunPrivilegedHelper
	helperGetEUID       = os.Geteuid
	helperReadFile      = os.ReadFile
)

func privHelperCmd() *cobra.Command {
	var cfg wireguard.HelperConfig
	var token string
	var tokenFile string

	cmd := &cobra.Command{
		Use:     "helper",
		Aliases: []string{"priv-helper"},
		Short:   "Run privileged network helper",
		Hidden:  true,
		RunE: func(cmd *cobra.Command, args []string) error {
			if helperGetEUID() != 0 {
				return fmt.Errorf("helper requires root")
			}

			resolvedToken, err := resolvePrivilegedToken(token, tokenFile)
			if err != nil {
				return err
			}
			cfg.Token = resolvedToken

			if cfg.MTU <= 0 {
				return fmt.Errorf("invalid mtu %d", cfg.MTU)
			}

			return runPrivilegedHelper(cmd.Context(), cfg)
		},
	}

	cmd.Flags().StringVar(&cfg.SocketPath, "socket", wireguard.DefaultPrivilegedSocketPath(), "Privileged helper unix socket path")
	cmd.Flags().StringVar(&token, "token", "", "Shared secret for helper requests (deprecated)")
	cmd.Flags().StringVar(&tokenFile, "token-file", wireguard.DefaultPrivilegedTokenPath(), "Path to privileged helper token file")
	cmd.Flags().StringVar(&cfg.TUNSocketPath, "tun-socket", wireguard.DefaultTUNSocketPath(), "Unix socket path used for TUN fd passing")
	cmd.Flags().IntVar(&cfg.MTU, "mtu", defaultHelperMTU, "TUN interface MTU")
	_ = cmd.Flags().MarkHidden("token")

	return cmd
}

func resolvePrivilegedToken(token, tokenFile string) (string, error) {
	if tok := strings.TrimSpace(token); tok != "" {
		return tok, nil
	}

	path := strings.TrimSpace(tokenFile)
	if path == "" {
		return "", fmt.Errorf("helper token is required; set --token or --token-file")
	}

	data, err := helperReadFile(path)
	if err != nil {
		return "", fmt.Errorf("read helper token file: %w", err)
	}
	tok := strings.TrimSpace(string(data))
	if tok == "" {
		return "", fmt.Errorf("helper token file is empty: %s", path)
	}
	return tok, nil
}
