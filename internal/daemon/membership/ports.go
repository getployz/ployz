package membership

import (
	"context"
	"net/netip"

	"ployz/internal/daemon/overlay"
)

type StateReader interface {
	LoadState(dataDir string) (*overlay.State, error)
}

type PeerApplier interface {
	ApplyPeerConfig(ctx context.Context, cfg overlay.Config, state *overlay.State, peers []Peer) error
}

type MachineRegistry interface {
	EnsureMachineTable(ctx context.Context) error
	UpsertMachine(ctx context.Context, row MachineRow, expectedVersion int64) error
	DeleteByEndpointExceptID(ctx context.Context, endpoint, id string) error
	DeleteMachine(ctx context.Context, machineID string) error
	ListMachineRows(ctx context.Context) ([]MachineRow, error)
	SubscribeMachines(ctx context.Context) ([]MachineRow, <-chan MachineChange, error)
}

type HeartbeatRegistry interface {
	EnsureHeartbeatTable(ctx context.Context) error
	SubscribeHeartbeats(ctx context.Context) ([]HeartbeatRow, <-chan HeartbeatChange, error)
	BumpHeartbeat(ctx context.Context, nodeID string, updatedAt string) error
}

type NetworkConfigRegistry interface {
	EnsureNetworkConfigTable(ctx context.Context) error
	EnsureNetworkCIDR(ctx context.Context, requested netip.Prefix, fallbackCIDR string, defaultCIDR netip.Prefix) (netip.Prefix, error)
}

type Registry interface {
	MachineRegistry
	HeartbeatRegistry
	NetworkConfigRegistry
}

type RegistryFactory func(addr netip.AddrPort, token string) Registry

type Controller interface {
	Reconcile(ctx context.Context, cfg overlay.Config) (int, error)
	ReconcilePeers(ctx context.Context, cfg overlay.Config, rows []MachineRow) (int, error)
	ListMachines(ctx context.Context, cfg overlay.Config) ([]Machine, error)
	UpsertMachine(ctx context.Context, cfg overlay.Config, m Machine) error
	RemoveMachine(ctx context.Context, cfg overlay.Config, machineID string) error
	Close() error
}
