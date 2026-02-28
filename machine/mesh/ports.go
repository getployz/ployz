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

// StoreRuntime manages the distributed state store process lifecycle.
// In production this is Corrosion running as a container, child process,
// or external service. Start must ensure the process is up and the schema
// is applied before returning (see corrorun.WaitReady).
type StoreRuntime interface {
	Start(ctx context.Context) error
	Stop(ctx context.Context) error
}

// ClusterStore is the distributed database backing the machine network.
// It becomes usable after StoreRuntime.Start returns — there is no
// separate Connect step because WaitReady already ensures readiness.
type ClusterStore interface {
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
