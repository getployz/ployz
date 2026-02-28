//go:build darwin

package platform

import (
	"ployz"
	"ployz/platform/wguser"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const wgInterface = "utun"

// NewWireGuard creates a userspace WireGuard implementation for macOS.
func NewWireGuard(key wgtypes.Key) *wguser.WG {
	mgmtIP := ployz.ManagementIPFromKey(key.PublicKey())

	return wguser.New(wguser.Config{
		Interface:  wgInterface,
		MTU:        WireGuardMTU,
		PrivateKey: key,
		Port:       WireGuardPort,
		MgmtIP:     mgmtIP,
		MgmtCIDR:   ployz.ManagementCIDR,
	}, nil, nil) // TODO: wire TUNProvider and PrivilegedRunner
}
