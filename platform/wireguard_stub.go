//go:build !linux && !darwin

package platform

import (
	"ployz/infra/wireguard/stub"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const (
	PrivilegedSocketPath = ""
	PrivilegedTokenPath  = ""
)

// NewWireGuard returns a stub WireGuard that errors on unsupported platforms.
func NewWireGuard(_ wgtypes.Key) *stub.WG {
	return stub.New()
}
