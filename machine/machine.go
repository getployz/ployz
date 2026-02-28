package machine

import (
	"fmt"
	"sync"

	"ployz"
	"ployz/internal/support/buildinfo"
	"ployz/platform"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// NewProduction creates a machine with platform defaults.
// This is the production entrypoint; use NewMachine with explicit
// options for tests.
func NewProduction(dataDir string) (*Machine, error) {
	m, err := NewMachine(dataDir)
	if err != nil {
		return nil, err
	}
	if m.wireGuard == nil {
		m.wireGuard = platform.NewWireGuard(m.identity.PrivateKey)
	}
	return m, nil
}

// Identity is the minimum a machine needs to exist on a network.
// The public key and management IP are derived from the private key.
type Identity struct {
	PrivateKey wgtypes.Key
	Name       string
}

// Machine is a node. It owns local identity and orchestrates the network
// components (WireGuard, cluster store, convergence) when the network is enabled.
type Machine struct {
	identity Identity
	dataDir  string

	wireGuard    WireGuard
	storeRuntime StoreRuntime
	store        ClusterStore
	convergence  Convergence

	mu    sync.Mutex
	phase Phase

	// started is closed when the machine is ready to serve requests.
	started chan struct{}
}

// Option configures a Machine. Use these to inject test dependencies.
type Option func(*Machine)

// WithIdentity sets the machine's identity, skipping file I/O.
func WithIdentity(id Identity) Option {
	return func(m *Machine) {
		m.identity = id
	}
}

// WithWireGuard injects a WireGuard implementation.
func WithWireGuard(wg WireGuard) Option {
	return func(m *Machine) {
		m.wireGuard = wg
	}
}

// WithStoreRuntime injects a store runtime lifecycle.
func WithStoreRuntime(r StoreRuntime) Option {
	return func(m *Machine) {
		m.storeRuntime = r
	}
}

// WithClusterStore injects a distributed store implementation.
func WithClusterStore(s ClusterStore) Option {
	return func(m *Machine) {
		m.store = s
	}
}

// WithConvergence injects a convergence loop.
func WithConvergence(c Convergence) Option {
	return func(m *Machine) {
		m.convergence = c
	}
}

// NewMachine creates a machine rooted at dataDir. Identity is loaded from
// disk or generated on first run, unless injected via WithIdentity.
// No platform defaults are applied — use NewProduction for that.
func NewMachine(dataDir string, opts ...Option) (*Machine, error) {
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

// Phase returns the current network lifecycle phase.
func (m *Machine) Phase() Phase {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.phase
}

// Store returns the cluster store, or nil if none is configured.
func (m *Machine) Store() ClusterStore {
	return m.store
}

// Started returns a channel that is closed when the machine is ready.
func (m *Machine) Started() <-chan struct{} {
	return m.started
}

// Status returns the machine's runtime status.
func (m *Machine) Status() ployz.Machine {
	m.mu.Lock()
	defer m.mu.Unlock()
	return ployz.Machine{
		Name:         m.identity.Name,
		PublicKey:    m.identity.PrivateKey.PublicKey().String(),
		NetworkPhase: m.phase.String(),
		Version:      buildinfo.Version,
	}
}
