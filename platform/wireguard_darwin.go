//go:build darwin

package platform

import (
	"ployz"
	wgcontainer "ployz/infra/wireguard/container"

	"github.com/docker/docker/client"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// NewWireGuard creates a containerized WireGuard implementation for macOS.
// The WireGuard interface runs inside a Docker container (OrbStack VM)
// where the kernel WireGuard module is available.
func NewWireGuard(key wgtypes.Key, docker client.APIClient) *wgcontainer.WG {
	mgmtIP := ployz.ManagementIPFromKey(key.PublicKey())

	return wgcontainer.New(wgcontainer.Config{
		Interface:     "ployz0",
		MTU:           WireGuardMTU,
		PrivateKey:    key,
		Port:          WireGuardPort,
		MgmtIP:        mgmtIP,
		Image:         WireGuardImage,
		ContainerName: WireGuardContainerName,
		NetworkName:   MeshNetworkName,
	}, docker)
}
