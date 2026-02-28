//go:build linux

package platform

import (
	"ployz"
	"ployz/platform/wgkernel"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const wgInterface = "ployz0"

// NewWireGuard creates a kernel WireGuard implementation for Linux.
func NewWireGuard(key wgtypes.Key) *wgkernel.WG {
	mgmtIP := ployz.ManagementIPFromKey(key.PublicKey())

	return wgkernel.New(wgkernel.Config{
		Interface:  wgInterface,
		MTU:        WireGuardMTU,
		PrivateKey: key,
		Port:       WireGuardPort,
		MgmtIP:     mgmtIP,
	})
}
