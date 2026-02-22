package network

import (
	"context"
	"net/netip"
)

// Registry abstracts machine CRUD against Corrosion.
// Production: adapter/corrosion.Store
// Testing: in-memory fake
type Registry interface {
	EnsureMachineTable(ctx context.Context) error
	EnsureNetworkConfigTable(ctx context.Context) error
	EnsureNetworkCIDR(ctx context.Context, requested netip.Prefix, fallbackCIDR string, defaultCIDR netip.Prefix) (netip.Prefix, error)
	UpsertMachine(ctx context.Context, row MachineRow, expectedVersion int64) error
	DeleteByEndpointExceptID(ctx context.Context, endpoint string, id string) error
	DeleteMachine(ctx context.Context, machineID string) error
	ListMachineRows(ctx context.Context) ([]MachineRow, error)
}

// RegistryFactory creates a Registry from Corrosion connection details.
// Production: func(addr, token) Registry { return corrosion.NewStore(addr, token) }
// Testing: func(addr, token) Registry { return fakeRegistry }
type RegistryFactory func(addr netip.AddrPort, token string) Registry

// StateStore persists network state.
// Production: SQLite via the current loadState/saveState functions
// Testing: in-memory map
type StateStore interface {
	Load(dataDir string) (*State, error)
	Save(dataDir string, s *State) error
	Delete(dataDir string) error
}
