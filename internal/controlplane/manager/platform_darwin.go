//go:build darwin

package manager

import (
	"context"
	"fmt"

	"ployz/internal/adapter/wireguard"
)

func startPlatformServices(ctx context.Context) error {
	if err := wireguard.ListenForTUN(ctx, wireguard.DefaultTUNSocketPath()); err != nil {
		return fmt.Errorf("start wireguard tun listener: %w", err)
	}
	return nil
}
