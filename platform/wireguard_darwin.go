//go:build darwin

package platform

import (
	"ployz"
	wgcontainer "ployz/infra/wireguard/container"

	"github.com/docker/docker/client"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// NewWireGuard creates a containerized WireGuard implementation for macOS
// with an in-process overlay bridge. The WireGuard interface runs inside
// a Docker container; the bridge gives the daemon overlay access via netstack.
func NewWireGuard(key wgtypes.Key, docker client.APIClient) *BridgedWG {
	mgmtIP := ployz.ManagementIPFromKey(key.PublicKey())

	containerWG := wgcontainer.New(wgcontainer.Config{
		Interface:     "ployz0",
		MTU:           WireGuardMTU,
		PrivateKey:    key,
		Port:          WireGuardPort,
		MgmtIP:        mgmtIP,
		Image:         WireGuardImage,
		ContainerName: WireGuardContainerName,
		NetworkName:   MeshNetworkName,
		HostPort:      WireGuardPort,
	}, docker)

	return NewBridgedWG(containerWG, key)
}
