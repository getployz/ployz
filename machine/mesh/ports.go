package mesh

import (
	"context"

	"ployz"
)

// WireGuard manages the overlay network interface and peer configuration.
type WireGuard interface {
	Up(ctx context.Context) error
	SetPeers(ctx context.Context, peers []ployz.MachineRecord) error
	Down(ctx context.Context) error
}

// Store is the distributed database backing the machine network.
// In production this is Corrosion — the adapter manages both the process
// lifecycle (Start/Stop) and data access. Start must ensure the process
// is up and the schema is applied before returning.
type Store interface {
	Start(ctx context.Context) error
	Stop(ctx context.Context) error
	ListMachines(ctx context.Context) ([]ployz.MachineRecord, error)
	SubscribeMachines(ctx context.Context) ([]ployz.MachineRecord, <-chan ployz.MachineEvent, error)
	UpsertMachine(ctx context.Context, rec ployz.MachineRecord) error
	DeleteMachine(ctx context.Context, id string) error
}

// Convergence watches the cluster store and syncs WireGuard peers.
type Convergence interface {
	Start(ctx context.Context) error
	Stop() error
}
