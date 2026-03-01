package machine

import (
	"context"
	"fmt"

	"ployz"
	"ployz/machine/mesh"
	"ployz/internal/support/buildinfo"

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

// MeshBuilder creates a network stack for the given identity. Platform wiring
// sets this via WithMeshBuilder; tests inject fakes directly via WithMesh.
type MeshBuilder func(ctx context.Context, id Identity) (NetworkStack, error)

// Identity is the minimum a machine needs to exist on a network.
// The public key and management IP are derived from the private key.
type Identity struct {
	PrivateKey wgtypes.Key
	Name       string
}

// Machine is a node. It owns local identity and optionally participates
// in a mesh network. The mesh is nil when the machine is standalone.
type Machine struct {
	identity     Identity
	dataDir      string
	mesh       NetworkStack
	buildMesh  MeshBuilder

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

// WithMeshBuilder sets the function used to construct a mesh on InitNetwork
// or when restoring from a saved network config.
func WithMeshBuilder(b MeshBuilder) Option {
	return func(m *Machine) {
		m.buildMesh = b
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

// Mesh returns the network stack, or nil if standalone.
func (m *Machine) Mesh() NetworkStack {
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
