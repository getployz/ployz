package supervisor

import (
	"context"

	"ployz/internal/network"
)

// Registry abstracts Corrosion's machine/heartbeat storage.
// Production: adapter/corrosion.Store
// Testing: in-memory fake with simulated gossip/replication
type Registry interface {
	EnsureMachineTable(ctx context.Context) error
	EnsureHeartbeatTable(ctx context.Context) error
	SubscribeMachines(ctx context.Context) ([]network.MachineRow, <-chan network.MachineChange, error)
	SubscribeHeartbeats(ctx context.Context) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error)
	ListMachineRows(ctx context.Context) ([]network.MachineRow, error)
	BumpHeartbeat(ctx context.Context, nodeID string, updatedAt string) error
}

// PeerReconciler applies peer configuration changes.
// Production: *network.Controller
// Testing: fake that tracks peer state in memory
type PeerReconciler interface {
	ReconcilePeers(ctx context.Context, cfg network.Config, rows []network.MachineRow) (int, error)
	Close() error
}
