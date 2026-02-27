package supervisor

import (
	"context"

	"ployz/internal/network"
)

// MachineRegistry abstracts machine storage and subscriptions.
type MachineRegistry interface {
	EnsureMachineTable(ctx context.Context) error
	SubscribeMachines(ctx context.Context) ([]network.MachineRow, <-chan network.MachineChange, error)
	ListMachineRows(ctx context.Context) ([]network.MachineRow, error)
}

// HeartbeatRegistry abstracts heartbeat storage and subscriptions.
type HeartbeatRegistry interface {
	EnsureHeartbeatTable(ctx context.Context) error
	SubscribeHeartbeats(ctx context.Context) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error)
	BumpHeartbeat(ctx context.Context, nodeID string, updatedAt string) error
}

// Registry abstracts Corrosion's machine/heartbeat storage.
// Production: adapter/corrosion.Store
// Testing: in-memory fake with simulated gossip/replication
type Registry interface {
	MachineRegistry
	HeartbeatRegistry
}

// PeerReconciler applies peer configuration changes.
// Production: *network.Controller
// Testing: fake that tracks peer state in memory
type PeerReconciler interface {
	ReconcilePeers(ctx context.Context, cfg network.Config, rows []network.MachineRow) (int, error)
	Close() error
}
