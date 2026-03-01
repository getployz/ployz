// Package mesh owns the network stack lifecycle: WireGuard, Corrosion,
// and convergence. A machine toggles the mesh on and off; mesh handles
// startup ordering, rollback on failure, and reverse teardown.
package mesh

import "sync"

// Mesh is the network stack. It orchestrates WireGuard, the store
// (Corrosion), and convergence as a single unit.
//
// Start order:  WG up → store start → convergence start.
// Detach order: convergence stop (infra stays running).
// Destroy order: convergence stop → store stop → WG down.
//
// Mesh is a concrete struct, not an interface. Tests construct a real Mesh
// with fake leaf deps injected via With* options.
type Mesh struct {
	wireGuard   WireGuard
	store       Store
	convergence Convergence
	overlayNet  OverlayNet

	mu    sync.Mutex
	phase Phase
}

// Option configures a Mesh.
type Option func(*Mesh)

// WithWireGuard injects a WireGuard implementation.
func WithWireGuard(wg WireGuard) Option {
	return func(m *Mesh) { m.wireGuard = wg }
}

// WithStore injects a store implementation.
func WithStore(s Store) Option {
	return func(m *Mesh) { m.store = s }
}

// WithConvergence injects a convergence loop.
func WithConvergence(c Convergence) Option {
	return func(m *Mesh) { m.convergence = c }
}

// WithOverlayNet injects an overlay network dialer.
func WithOverlayNet(o OverlayNet) Option {
	return func(m *Mesh) { m.overlayNet = o }
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

// Store returns the store, or nil if not configured.
func (m *Mesh) Store() Store {
	return m.store
}

// OverlayNet returns the overlay network dialer, or nil if not configured.
func (m *Mesh) OverlayNet() OverlayNet {
	return m.overlayNet
}
