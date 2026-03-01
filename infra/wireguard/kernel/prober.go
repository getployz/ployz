//go:build linux

package kernel

import (
	"context"
	"fmt"
	"time"

	"golang.zx2c4.com/wireguard/wgctrl"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// PeerHandshakes returns the last handshake time for each WireGuard peer.
func (w *WG) PeerHandshakes(_ context.Context) (map[wgtypes.Key]time.Time, error) {
	wg, err := wgctrl.New()
	if err != nil {
		return nil, fmt.Errorf("create wireguard client: %w", err)
	}
	defer wg.Close()

	dev, err := wg.Device(w.cfg.Interface)
	if err != nil {
		return nil, fmt.Errorf("inspect wireguard device %q: %w", w.cfg.Interface, err)
	}

	result := make(map[wgtypes.Key]time.Time, len(dev.Peers))
	for _, p := range dev.Peers {
		result[p.PublicKey] = p.LastHandshakeTime
	}
	return result, nil
}
