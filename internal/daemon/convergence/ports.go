package convergence

import (
	"context"
	"net/netip"

	"ployz/internal/daemon/membership"
	"ployz/internal/daemon/overlay"
)

// MachineRegistry abstracts machine storage and subscriptions.
type MachineRegistry interface {
	EnsureMachineTable(ctx context.Context) error
	UpsertMachine(ctx context.Context, row overlay.MachineRow, expectedVersion int64) error
	SubscribeMachines(ctx context.Context) ([]membership.MachineRow, <-chan membership.MachineChange, error)
	ListMachineRows(ctx context.Context) ([]membership.MachineRow, error)
}

// HeartbeatRegistry abstracts heartbeat storage and subscriptions.
type HeartbeatRegistry interface {
	EnsureHeartbeatTable(ctx context.Context) error
	SubscribeHeartbeats(ctx context.Context) ([]membership.HeartbeatRow, <-chan membership.HeartbeatChange, error)
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
// Production: membership.Service
// Testing: fake that tracks peer state in memory
type PeerReconciler interface {
	ReconcilePeers(ctx context.Context, cfg overlay.Config, rows []membership.MachineRow) (int, error)
	Close() error
}

type NetworkController interface {
	Start(ctx context.Context, cfg overlay.Config) (overlay.Config, error)
	Close() error
}

type NetworkControllerFactory func() (NetworkController, error)

type PeerReconcilerFactory func() (PeerReconciler, error)

type RegistryFactory func(addr netip.AddrPort, token string) Registry
