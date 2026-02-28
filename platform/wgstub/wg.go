//go:build !linux && !darwin

package wgstub

import (
	"context"
	"fmt"
	"runtime"

	"ployz"
)

// WG is a no-op WireGuard implementation for unsupported platforms.
type WG struct{}

func New() *WG { return &WG{} }

func (w *WG) Up(_ context.Context) error {
	return fmt.Errorf("wireguard not supported on %s", runtime.GOOS)
}

func (w *WG) SetPeers(_ context.Context, _ []ployz.MachineRecord) error {
	return fmt.Errorf("wireguard not supported on %s", runtime.GOOS)
}

func (w *WG) Down(_ context.Context) error { return nil }
