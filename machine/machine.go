package machine

import (
	"context"
	"fmt"
	"sync"

	"ployz"
	"ployz/internal/support/buildinfo"
	"ployz/machine/mesh"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// NetworkStack is the mesh network lifecycle seen by Machine.
// *mesh.Mesh satisfies this interface; tests inject a simple fake.
type NetworkStack interface {
	Up(ctx context.Context) error
	Detach(ctx context.Context) error
	Destroy(ctx context.Context) error
	Phase() mesh.Phase
	Store() mesh.Store
}

// MeshBuilder creates a network stack. The identity and data directory are
// captured in the closure at construction time.
type MeshBuilder func(ctx context.Context) (NetworkStack, error)

// Identity is the minimum a machine needs to exist on a network.
// The public key and management IP are derived from the private key.
type Identity struct {
	PrivateKey wgtypes.Key
	Name       string
}

// Machine is a node. It owns local identity and optionally participates
// in a mesh network. The mesh is nil when the machine is standalone.
type Machine struct {
	identity Identity
	dataDir  string

	mu   sync.Mutex
	mesh NetworkStack

	// started is closed when the machine is ready to serve requests.
	started chan struct{}
}

// Option configures a Machine.
type Option func(*Machine)

// WithIdentity sets the machine's identity, skipping file I/O.
func WithIdentity(id Identity) Option {
	return func(m *Machine) {
		m.identity = id
	}
}

// WithMesh attaches a network stack to the machine.
func WithMesh(ns NetworkStack) Option {
	return func(m *Machine) {
		m.mesh = ns
	}
}

// New creates a machine rooted at dataDir. Identity is loaded from
// disk or generated on first run, unless injected via WithIdentity.
func New(dataDir string, opts ...Option) (*Machine, error) {
	m := &Machine{
		dataDir: dataDir,
		started: make(chan struct{}),
	}
	for _, opt := range opts {
		opt(m)
	}

	if m.identity.PrivateKey == (wgtypes.Key{}) {
		id, err := loadOrCreateIdentity(dataDir)
		if err != nil {
			return nil, fmt.Errorf("load identity: %w", err)
		}
		m.identity = id
	}

	return m, nil
}

// Identity returns the machine's identity.
func (m *Machine) Identity() Identity {
	return m.identity
}

// SetMesh attaches a pre-built network stack. Called by the daemon
// before Run when restoring from a saved network config.
func (m *Machine) SetMesh(ns NetworkStack) {
	m.setMesh(ns)
}

// HasMeshAttached reports whether a mesh is currently attached.
// This is a factual check for preflight guards — not authoritative for races.
func (m *Machine) HasMeshAttached() bool {
	return m.getMesh() != nil
}

// Phase returns the current network lifecycle phase.
// Returns "stopped" if the machine has no mesh.
func (m *Machine) Phase() mesh.Phase {
	ns := m.getMesh()
	if ns == nil {
		return mesh.PhaseStopped
	}
	return ns.Phase()
}

// Store returns the mesh store, or nil if standalone or not configured.
func (m *Machine) Store() mesh.Store {
	ns := m.getMesh()
	if ns == nil {
		return nil
	}
	return ns.Store()
}

func (m *Machine) getMesh() NetworkStack {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.mesh
}

func (m *Machine) setMesh(ns NetworkStack) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.mesh = ns
}

// Started returns a channel that is closed when the machine is ready.
func (m *Machine) Started() <-chan struct{} {
	return m.started
}

// Status returns the machine's runtime status.
func (m *Machine) Status() ployz.Machine {
	return ployz.Machine{
		Name:         m.identity.Name,
		PublicKey:    m.identity.PrivateKey.PublicKey().String(),
		NetworkPhase: m.Phase().String(),
		Version:      buildinfo.Version,
	}
}
