package machine

import (
	"fmt"

	"ployz"
	"ployz/machine/mesh"
	"ployz/platform/buildinfo"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

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
	mesh     *mesh.Mesh

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

// WithMesh attaches a mesh network stack to the machine.
func WithMesh(msh *mesh.Mesh) Option {
	return func(m *Machine) {
		m.mesh = msh
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

// Mesh returns the mesh network stack, or nil if standalone.
func (m *Machine) Mesh() *mesh.Mesh {
	return m.mesh
}

// Phase returns the current network lifecycle phase.
// Returns "stopped" if the machine has no mesh.
func (m *Machine) Phase() mesh.Phase {
	if m.mesh == nil {
		return mesh.PhaseStopped
	}
	return m.mesh.Phase()
}

// Store returns the mesh store, or nil if standalone or not configured.
func (m *Machine) Store() mesh.Store {
	if m.mesh == nil {
		return nil
	}
	return m.mesh.Store()
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
