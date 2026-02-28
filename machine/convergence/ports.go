package convergence

import (
	"context"

	"ployz"
)

// PeerPlanner decides which machines become WireGuard peers for the local node.
type PeerPlanner interface {
	PlanPeers(self ployz.MachineRecord, all []ployz.MachineRecord) []ployz.MachineRecord
}

// Subscriber provides a subscription to the current set of machines.
// mesh.Store satisfies this interface.
type Subscriber interface {
	SubscribeMachines(ctx context.Context) ([]ployz.MachineRecord, <-chan ployz.MachineEvent, error)
}

// PeerSetter applies the desired WireGuard peer set.
type PeerSetter interface {
	SetPeers(ctx context.Context, peers []ployz.MachineRecord) error
}
