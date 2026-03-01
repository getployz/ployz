//go:build darwin

package wireguard

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"strings"
	"time"

	"golang.zx2c4.com/wireguard/tun"
)

const (
	helperProvisionRetryWait = 200 * time.Millisecond
	helperProvisionLogEvery  = 25
)

// RunPrivilegedHelper starts the macOS privileged helper server.
// It creates a TUN device and provisions the fd to the daemon, then
// accepts privileged command requests (ifconfig, route) over a unix socket.
func RunPrivilegedHelper(ctx context.Context, cfg HelperConfig) error {
	path := strings.TrimSpace(cfg.SocketPath)
	tok := strings.TrimSpace(cfg.Token)
	tunSocketPath := strings.TrimSpace(cfg.TUNSocketPath)
	if path == "" {
		return fmt.Errorf("privileged helper socket path is required")
	}
	if tok == "" {
		return fmt.Errorf("privileged helper token is required")
	}
	if tunSocketPath == "" {
		return fmt.Errorf("tun socket path is required")
	}
	if cfg.MTU <= 0 {
		return fmt.Errorf("invalid tun mtu %d", cfg.MTU)
	}
	cfg.SocketPath = path
	cfg.Token = tok
	cfg.TUNSocketPath = tunSocketPath

	startHook := func(ctx context.Context, log *slog.Logger) (func(), error) {
		return startTUNProvisionLoop(ctx, cfg, log)
	}

	return runHelperServer(ctx, path, tok, validateDarwinCommand, startHook)
}

func validateDarwinCommand(name string, args []string) error {
	if name == "" {
		return fmt.Errorf("command name is required")
	}
	switch name {
	case "ifconfig", "route":
		// allowed
	default:
		return fmt.Errorf("command %q is not allowed", name)
	}
	return validateCommandArgs(args)
}

func startTUNProvisionLoop(ctx context.Context, cfg HelperConfig, log *slog.Logger) (func(), error) {
	tunDev, tunFile, tunName, err := newProvisionedTUN(cfg.MTU)
	if err != nil {
		return nil, err
	}
	log.Info("privileged helper tun ready", "iface", tunName, "mtu", cfg.MTU)

	provisionCtx, cancel := context.WithCancel(ctx)
	go provisionTUNUntilReady(provisionCtx, cfg, tunFile, tunName, log)

	return func() {
		cancel()
		_ = tunDev.Close()
	}, nil
}

func newProvisionedTUN(mtu int) (tun.Device, *os.File, string, error) {
	tunDev, err := tun.CreateTUN("utun", mtu)
	if err != nil {
		return nil, nil, "", fmt.Errorf("create tun device: %w", err)
	}

	tunName, err := tunDev.Name()
	if err != nil {
		_ = tunDev.Close()
		return nil, nil, "", fmt.Errorf("get tun interface name: %w", err)
	}

	fileBacked, ok := tunDev.(interface{ File() *os.File })
	if !ok {
		_ = tunDev.Close()
		return nil, nil, "", fmt.Errorf("tun implementation does not expose file descriptor: %T", tunDev)
	}
	tunFile := fileBacked.File()
	if tunFile == nil {
		_ = tunDev.Close()
		return nil, nil, "", fmt.Errorf("tun file descriptor is nil")
	}

	return tunDev, tunFile, tunName, nil
}

func provisionTUNUntilReady(ctx context.Context, cfg HelperConfig, tunFile *os.File, tunName string, log *slog.Logger) {
	attempts := 0
	for {
		err := SendTUN(cfg.TUNSocketPath, tunFile, tunName, cfg.MTU, cfg.SocketPath, cfg.Token)
		if err == nil {
			log.Info("provisioned tun descriptor", "iface", tunName, "socket", cfg.TUNSocketPath)
			return
		}

		attempts++
		if !isTransientSocketError(err) {
			log.Warn("send tun descriptor failed", "err", err)
		} else if attempts == 1 || attempts%helperProvisionLogEvery == 0 {
			log.Debug("waiting for daemon tun listener", "socket", cfg.TUNSocketPath, "err", err)
		}

		select {
		case <-ctx.Done():
			return
		case <-time.After(helperProvisionRetryWait):
		}
	}
}
