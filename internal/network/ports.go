package network

import (
	"context"
	"net/netip"
	"time"
)

// Clock abstracts time.Now() for deterministic testing.
type Clock interface {
	Now() time.Time
}

// RealClock implements Clock using the real system clock.
type RealClock struct{}

func (RealClock) Now() time.Time { return time.Now() }

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
// Production: adapter/sqlite.NetworkStateStore
// Testing: in-memory map
type StateStore interface {
	Load(dataDir string) (*State, error)
	Save(dataDir string, s *State) error
	Delete(dataDir string) error
	StatePath(dataDir string) string
}

// ContainerRuntime abstracts container engine operations.
// Production: adapter/docker.Runtime (wrapping Docker *client.Client)
// Future: Podman, containerd, or test fake
type ContainerRuntime interface {
	// Daemon health
	WaitReady(ctx context.Context) error

	// Container lifecycle
	ContainerInspect(ctx context.Context, name string) (ContainerInfo, error)
	ContainerStart(ctx context.Context, name string) error
	ContainerStop(ctx context.Context, name string) error
	ContainerRemove(ctx context.Context, name string, force bool) error
	ContainerLogs(ctx context.Context, name string, lines int) (string, error)
	ContainerCreate(ctx context.Context, cfg ContainerCreateConfig) error
	ImagePull(ctx context.Context, image string) error

	// Overlay network
	NetworkInspect(ctx context.Context, name string) (NetworkInfo, error)
	NetworkCreate(ctx context.Context, name string, subnet netip.Prefix, wgIface string) error
	NetworkRemove(ctx context.Context, name string) error

	Close() error
}

// ContainerInfo describes the state of a container.
type ContainerInfo struct {
	Exists  bool
	Running bool
}

// NetworkInfo describes the state of a container network.
type NetworkInfo struct {
	ID     string
	Subnet string
	Exists bool
}

// ContainerCreateConfig holds parameters for creating a container.
type ContainerCreateConfig struct {
	Name        string
	Image       string
	Cmd         []string
	Env         []string
	NetworkMode string
	User        string
	Mounts      []Mount
}

// Mount describes a bind mount for a container.
type Mount struct {
	Source   string
	Target  string
	ReadOnly bool
}

// CorrosionRuntime manages the Corrosion container lifecycle.
// Production: adapter/corrosion/container.Adapter
// Testing: fake that records calls
type CorrosionRuntime interface {
	WriteConfig(cfg CorrosionConfig) error
	Start(ctx context.Context, cfg CorrosionConfig) error
	Stop(ctx context.Context, name string) error
}

// StatusProber probes infrastructure components for the Status check.
// Production: darwinStatusProber / linuxStatusProber
// Testing: fake that returns fixed values
type StatusProber interface {
	ProbeInfra(ctx context.Context, state *State) (wg bool, dockerNet bool, corrosion bool, err error)
}

// PlatformOps encapsulates platform-specific runtime operations.
// Production: adapter/platform.LinuxPlatformOps / DarwinPlatformOps
// Testing: fake
type PlatformOps interface {
	Prepare(ctx context.Context, cfg Config, state StateStore) error
	ConfigureWireGuard(ctx context.Context, cfg Config, state *State) error
	EnsureDockerNetwork(ctx context.Context, cfg Config, state *State) error
	CleanupDockerNetwork(ctx context.Context, cfg Config, state *State) error
	CleanupWireGuard(ctx context.Context, cfg Config, state *State) error
	AfterStart(ctx context.Context, cfg Config) error
	AfterStop(ctx context.Context, cfg Config, state *State) error
	ApplyPeerConfig(ctx context.Context, cfg Config, state *State, peers []Peer) error
}

// CorrosionConfig is the unified config for Corrosion lifecycle ops.
type CorrosionConfig struct {
	Name         string
	Image        string
	Dir          string
	ConfigPath   string
	DataDir      string
	AdminSock    string
	Bootstrap    []string
	GossipAddr   netip.AddrPort
	MemberID     uint64
	APIAddr      netip.AddrPort
	APIToken     string
	GossipMaxMTU int
	User         string
}
