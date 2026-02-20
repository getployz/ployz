//go:build !darwin

package main

import (
	"context"
	"fmt"
	"net/netip"
)

func startHostAccessSession(
	_ context.Context,
	_ string,
	_ string,
	_ netip.Addr,
	_ string,
	_ netip.AddrPort,
	_ string,
) (hostAccessSession, error) {
	return nil, fmt.Errorf("host access is currently supported on macOS only")
}
