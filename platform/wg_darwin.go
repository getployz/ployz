//go:build darwin

package platform

import (
	"ployz"
	"ployz/infra/wireguard/user"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const wgInterface = "utun"

// NewWireGuard creates a userspace WireGuard implementation for macOS.
func NewWireGuard(key wgtypes.Key) *user.WG {
	mgmtIP := ployz.ManagementIPFromKey(key.PublicKey())

	return user.New(user.Config{
		Interface:  wgInterface,
		MTU:        WireGuardMTU,
		PrivateKey: key,
		Port:       WireGuardPort,
		MgmtIP:     mgmtIP,
		MgmtCIDR:   ployz.ManagementCIDR,
	}, nil, nil) // TODO: wire TUNProvider and PrivilegedRunner
}
