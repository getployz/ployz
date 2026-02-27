package membership

import (
	"net/netip"

	"ployz/internal/daemon/overlay"
)

type Peer = overlay.Peer
type PeerSpec = overlay.PeerSpec

func BuildPeerSpecs(peers []Peer) ([]PeerSpec, error) {
	return overlay.BuildPeerSpecs(peers)
}

func SingleIPPrefix(addr netip.Addr) netip.Prefix {
	return overlay.SingleIPPrefix(addr)
}

func MachineIP(subnet netip.Prefix) netip.Addr {
	return overlay.MachineIP(subnet)
}
