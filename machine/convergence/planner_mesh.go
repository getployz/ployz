package convergence

import "ployz"

// MeshPlanner peers with every other machine in the network.
// Suitable for small clusters (under ~50 nodes).
type MeshPlanner struct{}

// PlanPeers returns all machines except self.
func (MeshPlanner) PlanPeers(self ployz.MachineRecord, all []ployz.MachineRecord) []ployz.MachineRecord {
	peers := make([]ployz.MachineRecord, 0, len(all))
	for _, machine := range all {
		if machine.PublicKey == self.PublicKey {
			continue
		}
		peers = append(peers, machine)
	}
	return peers
}
