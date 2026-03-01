package platform

import (
	"net/netip"

	"ployz/internal/daemon/overlay"
	"ployz/internal/infra/wireguard"
)

var defaultNetworkPrefix = netip.MustParsePrefix("10.210.0.0/16")

const defaultWireGuardMTU = 1280

func peerConfigsFromSpecs(specs []overlay.PeerSpec) []wireguard.PeerConfig {
	peers := make([]wireguard.PeerConfig, len(specs))
	for i := range specs {
		spec := specs[i]
		peers[i] = wireguard.PeerConfig{
			PublicKey:       spec.PublicKey,
			Endpoint:        spec.Endpoint,
			AllowedPrefixes: spec.AllowedPrefixes,
		}
	}

	return peers
}
