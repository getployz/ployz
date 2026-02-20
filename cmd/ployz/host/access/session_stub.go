//go:build !darwin

package access

import (
	"context"
	"fmt"
	"net/netip"
)

func startSession(
	_ context.Context,
	_ string,
	_ string,
	_ netip.Addr,
	_ string,
	_ netip.AddrPort,
	_ string,
) (session, error) {
	return nil, fmt.Errorf("host access is currently supported on macOS only")
}
