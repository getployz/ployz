//go:build linux

package wireguard

import (
	"context"
	"fmt"
	"strings"
)

// RunPrivilegedHelper starts the Linux privileged helper server.
// It accepts privileged command requests (ip, wg) over a unix socket.
func RunPrivilegedHelper(ctx context.Context, cfg HelperConfig) error {
	path := strings.TrimSpace(cfg.SocketPath)
	tok := strings.TrimSpace(cfg.Token)
	if path == "" {
		return fmt.Errorf("privileged helper socket path is required")
	}
	if tok == "" {
		return fmt.Errorf("privileged helper token is required")
	}

	return runHelperServer(ctx, path, tok, validateLinuxCommand, nil)
}

func validateLinuxCommand(name string, args []string) error {
	if name == "" {
		return fmt.Errorf("command name is required")
	}
	switch name {
	case "ip", "wg":
		// allowed
	default:
		return fmt.Errorf("command %q is not allowed", name)
	}
	return validateCommandArgs(args)
}
