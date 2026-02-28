package machine

import (
	"context"

	"ployz"
)

// ClusterStore is the distributed database backing the machine network.
// In production this is Corrosion; in tests it can be a fake with
// tunable replication latency.
type ClusterStore interface {
	ListMachines(ctx context.Context) ([]ployz.MachineRecord, error)
	SubscribeMachines(ctx context.Context) ([]ployz.MachineRecord, <-chan ployz.MachineEvent, error)
	UpsertMachine(ctx context.Context, rec ployz.MachineRecord) error
	DeleteMachine(ctx context.Context, id string) error
}

// WireGuard manages the overlay network interface and peer configuration.
type WireGuard interface {
	Up(ctx context.Context) error
	SetPeers(ctx context.Context, peers []ployz.MachineRecord) error
	Down(ctx context.Context) error
}

// StoreRuntime manages the distributed state store process lifecycle.
type StoreRuntime interface {
	Start(ctx context.Context) error
	Stop(ctx context.Context) error
}

// Convergence watches the cluster store and syncs WireGuard peers.
// It owns its own goroutine lifecycle.
type Convergence interface {
	Start(ctx context.Context) error
	Stop() error
}
