package container

import (
	"context"
	"fmt"
	"strconv"
	"strings"
	"time"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// PeerHandshakes returns the last handshake time for each WireGuard peer
// by running "wg show <iface> latest-handshakes" inside the container.
func (w *WG) PeerHandshakes(ctx context.Context) (map[wgtypes.Key]time.Time, error) {
	out, err := exec(ctx, w.docker, w.cfg.ContainerName,
		"wg", "show", w.cfg.Interface, "latest-handshakes",
	)
	if err != nil {
		return nil, fmt.Errorf("wg show latest-handshakes: %w", err)
	}

	result := make(map[wgtypes.Key]time.Time)
	for _, line := range splitLines(out) {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		// Format: "<pubkey>\t<unix_timestamp>"
		parts := strings.SplitN(line, "\t", 2)
		if len(parts) != 2 {
			continue
		}

		key, err := wgtypes.ParseKey(parts[0])
		if err != nil {
			continue
		}

		epoch, err := strconv.ParseInt(strings.TrimSpace(parts[1]), 10, 64)
		if err != nil {
			continue
		}

		// Timestamp 0 means no handshake has occurred.
		if epoch == 0 {
			result[key] = time.Time{}
		} else {
			result[key] = time.Unix(epoch, 0)
		}
	}

	return result, nil
}
