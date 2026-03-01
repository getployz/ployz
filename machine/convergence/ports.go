package convergence

import (
	"context"
	"time"

	"ployz"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
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

// PeerProber retrieves the last handshake time for each WireGuard peer.
type PeerProber interface {
	PeerHandshakes(ctx context.Context) (map[wgtypes.Key]time.Time, error)
}
