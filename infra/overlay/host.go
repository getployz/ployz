// Package overlay provides host-level access to the WireGuard overlay network.
package overlay

import (
	"context"
	"net"
)

// Host uses the system network stack directly. Used on Linux where
// kernel WireGuard makes overlay IPs natively routable from the host.
type Host struct{}

// DialContext dials through the host network stack.
func (Host) DialContext(ctx context.Context, network, addr string) (net.Conn, error) {
	return (&net.Dialer{}).DialContext(ctx, network, addr)
}
