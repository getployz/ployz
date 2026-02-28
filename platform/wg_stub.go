//go:build !linux && !darwin

package platform

import (
	"ployz/platform/wgstub"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// NewWireGuard returns a stub WireGuard that errors on unsupported platforms.
func NewWireGuard(_ wgtypes.Key) *wgstub.WG {
	return wgstub.New()
}
