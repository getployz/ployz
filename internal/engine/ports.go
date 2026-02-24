package engine

import (
	"context"
	"net/netip"

	"ployz/internal/network"
	"ployz/internal/reconcile"
)

// NetworkController manages a network's imperative lifecycle (start/stop).
// Production: *network.Controller
// Testing: fake that tracks start/stop state
type NetworkController interface {
	Start(ctx context.Context, cfg network.Config) (network.Config, error)
	Close() error
}

// NetworkControllerFactory creates controllers for starting networks.
// Production: returns *network.Controller with real Docker client
// Testing: returns fake controller
type NetworkControllerFactory func() (NetworkController, error)

// PeerReconcilerFactory creates peer reconcilers for continuous reconciliation.
// Production: returns *network.Controller
// Testing: returns fake reconciler
type PeerReconcilerFactory func() (reconcile.PeerReconciler, error)

// RegistryFactory creates a Registry from Corrosion connection details.
// Production: func(addr, token) { return corrosion.NewStore(addr, token) }
// Testing: returns in-memory fake
type RegistryFactory func(addr netip.AddrPort, token string) reconcile.Registry
