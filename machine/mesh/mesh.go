// Package mesh owns the network stack lifecycle: WireGuard, Corrosion,
// and convergence. A machine toggles the mesh on and off; mesh handles
// startup ordering, rollback on failure, and reverse teardown.
package mesh

import "sync"

// Mesh is the network stack. It orchestrates WireGuard, the store runtime
// (Corrosion), and convergence as a single unit.
//
// Start order:  WG up → store runtime start → convergence start.
// Stop order:   convergence stop → store runtime stop → WG down.
//
// Mesh is a concrete struct, not an interface. Tests construct a real Mesh
// with fake leaf deps injected via With* options.
type Mesh struct {
	wireGuard    WireGuard
	storeRuntime StoreRuntime
	store        ClusterStore
	convergence  Convergence

	mu    sync.Mutex
	phase Phase
}

// Option configures a Mesh.
type Option func(*Mesh)

// WithWireGuard injects a WireGuard implementation.
func WithWireGuard(wg WireGuard) Option {
	return func(m *Mesh) { m.wireGuard = wg }
}

// WithStoreRuntime injects a store runtime lifecycle.
func WithStoreRuntime(r StoreRuntime) Option {
	return func(m *Mesh) { m.storeRuntime = r }
}

// WithClusterStore injects a cluster store implementation.
func WithClusterStore(s ClusterStore) Option {
	return func(m *Mesh) { m.store = s }
}

// WithConvergence injects a convergence loop.
func WithConvergence(c Convergence) Option {
	return func(m *Mesh) { m.convergence = c }
}

// New creates a Mesh with the given options.
func New(opts ...Option) *Mesh {
	m := &Mesh{}
	for _, opt := range opts {
		opt(m)
	}
	return m
}

// Phase returns the current network lifecycle phase.
func (m *Mesh) Phase() Phase {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.phase
}

// Store returns the cluster store, or nil if not configured.
func (m *Mesh) Store() ClusterStore {
	return m.store
}
