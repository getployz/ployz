package convergence

import (
	"net/netip"
	"slices"

	"ployz"
)

// MeshPlanner peers with every other machine in the network.
// Suitable for small clusters (under ~50 nodes).
type MeshPlanner struct{}

// PlanPeers returns copies of all machines except self, with endpoints
// sorted so private/link-local IPs come before public IPs.
func (MeshPlanner) PlanPeers(self ployz.MachineRecord, all []ployz.MachineRecord) []ployz.MachineRecord {
	peers := make([]ployz.MachineRecord, 0, len(all))
	for _, machine := range all {
		if machine.PublicKey == self.PublicKey {
			continue
		}
		if len(machine.Endpoints) > 1 {
			// Copy to avoid mutating the original.
			sorted := make([]netip.AddrPort, len(machine.Endpoints))
			copy(sorted, machine.Endpoints)
			slices.SortStableFunc(sorted, compareEndpoints)
			machine.Endpoints = sorted
		}
		peers = append(peers, machine)
	}
	return peers
}

// compareEndpoints orders private/link-local IPs before public IPs.
func compareEndpoints(a, b netip.AddrPort) int {
	aPrivate := a.Addr().IsPrivate() || a.Addr().IsLinkLocalUnicast()
	bPrivate := b.Addr().IsPrivate() || b.Addr().IsLinkLocalUnicast()
	switch {
	case aPrivate && !bPrivate:
		return -1
	case !aPrivate && bPrivate:
		return 1
	default:
		return 0
	}
}
