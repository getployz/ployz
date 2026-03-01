//go:build !linux && !darwin

package stub

import (
	"context"
	"fmt"
	"runtime"
	"time"

	"ployz"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
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

// PeerHandshakes returns an empty map on unsupported platforms.
func (w *WG) PeerHandshakes(_ context.Context) (map[wgtypes.Key]time.Time, error) {
	return nil, nil
}
