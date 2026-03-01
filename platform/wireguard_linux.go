//go:build linux

package platform

import (
	"ployz"
	"ployz/infra/wireguard/kernel"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

const (
	wgInterface          = "ployz0"
	PrivilegedSocketPath = "/run/ployz/helper.sock"
	PrivilegedTokenPath  = "/var/lib/ployz/private/helper.token"
)

// NewWireGuard creates a kernel WireGuard implementation for Linux.
func NewWireGuard(key wgtypes.Key) *kernel.WG {
	mgmtIP := ployz.ManagementIPFromKey(key.PublicKey())

	return kernel.New(kernel.Config{
		Interface:  wgInterface,
		MTU:        WireGuardMTU,
		PrivateKey: key,
		Port:       WireGuardPort,
		MgmtIP:     mgmtIP,
	})
}
