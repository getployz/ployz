package reconcile

import (
	"context"

	"ployz/internal/mesh"
)

// Registry abstracts Corrosion's machine/heartbeat storage.
// Production: adapter/corrosion.Store
// Testing: in-memory fake with simulated gossip/replication
type Registry interface {
	EnsureMachineTable(ctx context.Context) error
	EnsureHeartbeatTable(ctx context.Context) error
	SubscribeMachines(ctx context.Context) ([]mesh.MachineRow, <-chan mesh.MachineChange, error)
	SubscribeHeartbeats(ctx context.Context) ([]mesh.HeartbeatRow, <-chan mesh.HeartbeatChange, error)
	ListMachineRows(ctx context.Context) ([]mesh.MachineRow, error)
	BumpHeartbeat(ctx context.Context, nodeID string, updatedAt string) error
}

// PeerReconciler applies peer configuration changes.
// Production: *mesh.Controller
// Testing: fake that tracks peer state in memory
type PeerReconciler interface {
	ReconcilePeers(ctx context.Context, cfg mesh.Config, rows []mesh.MachineRow) (int, error)
	Close() error
}
