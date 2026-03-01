package platform

const (
	WireGuardMTU           = 1420
	WireGuardPort          = 51820
	WireGuardImage         = "procustodibus/wireguard"
	WireGuardContainerName = "ployz-wireguard"
	MeshNetworkName        = "ployz-mesh"
	SocketName             = "ployz.sock"

	CorrosionImage         = "ghcr.io/psviderski/corrosion:latest"
	CorrosionContainerName = "ployz-corrosion"
)
