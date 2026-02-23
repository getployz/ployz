package scenario

import (
	"context"
	"fmt"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	"ployz/internal/adapter/fake/cluster"
	"ployz/internal/adapter/fake/leaf"
	"ployz/internal/check"

	"ployz/internal/daemon/supervisor"
	"ployz/internal/engine"
	"ployz/internal/mesh"
	"ployz/internal/reconcile"
)

const defaultDataRootBase = "/tmp/ployz-scenario"

// Config defines how a Scenario is composed.
type Config struct {
	NodeIDs      []string
	DataRootBase string
	Clock        mesh.Clock
}

// Node exposes one configured node and its test doubles.
type Node struct {
	ID               string
	DataRoot         string
	Manager          *supervisor.Manager
	PlatformOps      *leaf.PlatformOps
	StateStore       *leaf.StateStore
	SpecStore        *leaf.SpecStore
	ContainerRuntime *leaf.ContainerRuntime
	CorrosionRuntime *leaf.CorrosionRuntime
	StatusProber     *leaf.StatusProber
	cancel           context.CancelFunc
}

// Scenario provisions a set of manager nodes over a shared fake cluster.
type Scenario struct {
	Cluster *cluster.Cluster
	nodes   map[string]*Node
	ctx     context.Context
	clock   mesh.Clock
	dataDir string
}

type nodeDependencies struct {
	stateStore       *leaf.StateStore
	specStore        *leaf.SpecStore
	platformOps      *leaf.PlatformOps
	containerRuntime *leaf.ContainerRuntime
	corrosionRuntime *leaf.CorrosionRuntime
	statusProber     *leaf.StatusProber
}

// New creates a multi-node test scenario with fully wired managers.
func New(ctx context.Context, cfg Config) (*Scenario, error) {
	check.Assert(ctx != nil, "scenario.New: context must not be nil")
	if ctx == nil {
		return nil, fmt.Errorf("context is required")
	}

	if len(cfg.NodeIDs) == 0 {
		return nil, fmt.Errorf("node ids must not be empty")
	}
	if cfg.Clock == nil {
		cfg.Clock = mesh.RealClock{}
	}
	if strings.TrimSpace(cfg.DataRootBase) == "" {
		cfg.DataRootBase = defaultDataRootBase
	}

	cl := cluster.NewCluster(cfg.Clock)
	s := &Scenario{
		Cluster: cl,
		nodes:   make(map[string]*Node, len(cfg.NodeIDs)),
		ctx:     ctx,
		clock:   cfg.Clock,
		dataDir: cfg.DataRootBase,
	}

	for _, nodeID := range cfg.NodeIDs {
		if _, err := s.AddNode(nodeID); err != nil {
			return nil, fmt.Errorf("setup node %q: %w", nodeID, err)
		}
	}

	return s, nil
}

// MustNew is New but fails the test immediately on error.
func MustNew(t testing.TB, ctx context.Context, cfg Config) *Scenario {
	t.Helper()
	s, err := New(ctx, cfg)
	if err != nil {
		t.Fatalf("create scenario: %v", err)
	}
	return s
}

// Node returns a configured node by ID, or nil when absent.
func (s *Scenario) Node(id string) *Node {
	check.Assert(s != nil, "Scenario.Node: receiver must not be nil")
	if s == nil {
		return nil
	}
	return s.nodes[id]
}

// AddNode provisions a new node into the scenario.
func (s *Scenario) AddNode(nodeID string) (*Node, error) {
	check.Assert(s != nil, "Scenario.AddNode: receiver must not be nil")
	if s == nil {
		return nil, fmt.Errorf("scenario is required")
	}

	check.Assert(strings.TrimSpace(nodeID) != "", "Scenario.AddNode: node ID must not be empty")
	if strings.TrimSpace(nodeID) == "" {
		return nil, fmt.Errorf("node id must not be empty")
	}
	if _, exists := s.nodes[nodeID]; exists {
		return nil, fmt.Errorf("node %q already exists", nodeID)
	}

	// Ensure node exists for fan-out replication semantics.
	s.Cluster.Registry(nodeID)
	s.Cluster.RestartNode(nodeID)

	nodeCtx, cancel := context.WithCancel(s.ctx)
	node, err := newNode(nodeCtx, nodeID, filepath.Join(s.dataDir, nodeID), s.Cluster, s.clock)
	if err != nil {
		cancel()
		return nil, err
	}
	node.cancel = cancel
	s.nodes[nodeID] = node
	return node, nil
}

// RemoveNode stops a managed node and removes it from scenario accessors.
// Cluster state for the node is marked dead but preserved.
func (s *Scenario) RemoveNode(nodeID string) error {
	check.Assert(s != nil, "Scenario.RemoveNode: receiver must not be nil")
	if s == nil {
		return fmt.Errorf("scenario is required")
	}

	node, ok := s.nodes[nodeID]
	if !ok {
		return fmt.Errorf("node %q not found", nodeID)
	}
	if node.cancel != nil {
		node.cancel()
	}
	s.Cluster.KillNode(nodeID)
	delete(s.nodes, nodeID)
	return nil
}

// Nodes returns all configured node IDs in sorted order.
func (s *Scenario) Nodes() []string {
	check.Assert(s != nil, "Scenario.Nodes: receiver must not be nil")
	if s == nil {
		return nil
	}

	ids := make([]string, 0, len(s.nodes))
	for id := range s.nodes {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	return ids
}

// Drain flushes all pending cluster replication writes.
func (s *Scenario) Drain() {
	check.Assert(s != nil, "Scenario.Drain: receiver must not be nil")
	if s == nil {
		return
	}
	check.Assert(s.Cluster != nil, "Scenario.Drain: cluster must not be nil")
	if s.Cluster == nil {
		return
	}
	s.Cluster.Drain()
}

// Tick delivers pending cluster writes up to the scenario clock.
func (s *Scenario) Tick() {
	check.Assert(s != nil, "Scenario.Tick: receiver must not be nil")
	if s == nil {
		return
	}
	check.Assert(s.Cluster != nil, "Scenario.Tick: cluster must not be nil")
	if s.Cluster == nil {
		return
	}
	s.Cluster.Tick()
}

// SetLink configures replication behavior from one node to another.
func (s *Scenario) SetLink(from, to string, cfg cluster.LinkConfig) {
	check.Assert(s != nil, "Scenario.SetLink: receiver must not be nil")
	if s == nil {
		return
	}
	s.Cluster.SetLink(from, to, cfg)
}

// BlockLink blocks replication on a directed link.
func (s *Scenario) BlockLink(from, to string) {
	check.Assert(s != nil, "Scenario.BlockLink: receiver must not be nil")
	if s == nil {
		return
	}
	s.Cluster.BlockLink(from, to)
}

// Partition creates a bidirectional partition between node groups.
func (s *Scenario) Partition(groupA, groupB []string) {
	check.Assert(s != nil, "Scenario.Partition: receiver must not be nil")
	if s == nil {
		return
	}
	s.Cluster.Partition(groupA, groupB)
}

// Heal removes all active partitions in the cluster.
func (s *Scenario) Heal() {
	check.Assert(s != nil, "Scenario.Heal: receiver must not be nil")
	if s == nil {
		return
	}
	s.Cluster.Heal()
}

// KillNode marks a node dead in the cluster.
func (s *Scenario) KillNode(nodeID string) {
	check.Assert(s != nil, "Scenario.KillNode: receiver must not be nil")
	if s == nil {
		return
	}
	s.Cluster.KillNode(nodeID)
}

// RestartNode marks a killed node alive and runs anti-entropy sync.
func (s *Scenario) RestartNode(nodeID string) {
	check.Assert(s != nil, "Scenario.RestartNode: receiver must not be nil")
	if s == nil {
		return
	}
	s.Cluster.RestartNode(nodeID)
}

// Snapshot returns point-in-time state for a node.
func (s *Scenario) Snapshot(nodeID string) cluster.NodeSnapshot {
	check.Assert(s != nil, "Scenario.Snapshot: receiver must not be nil")
	if s == nil {
		return cluster.NodeSnapshot{}
	}
	return s.Cluster.Snapshot(nodeID)
}

func newNode(ctx context.Context, nodeID string, dataRoot string, cluster *cluster.Cluster, clock mesh.Clock) (*Node, error) {
	check.Assert(ctx != nil, "scenario.newNode: context must not be nil")
	if ctx == nil {
		return nil, fmt.Errorf("context is required")
	}
	check.Assert(cluster != nil, "scenario.newNode: cluster must not be nil")
	if cluster == nil {
		return nil, fmt.Errorf("cluster is required")
	}
	check.Assert(clock != nil, "scenario.newNode: clock must not be nil")
	if clock == nil {
		return nil, fmt.Errorf("clock is required")
	}
	check.Assert(strings.TrimSpace(nodeID) != "", "scenario.newNode: node ID must not be empty")
	if strings.TrimSpace(nodeID) == "" {
		return nil, fmt.Errorf("node id is required")
	}
	check.Assert(strings.TrimSpace(dataRoot) != "", "scenario.newNode: data root must not be empty")
	if strings.TrimSpace(dataRoot) == "" {
		return nil, fmt.Errorf("data root is required")
	}

	deps := newNodeDependencies()
	newController := newControllerFactory(nodeID, cluster, clock, deps)

	ctrl, err := newController()
	if err != nil {
		return nil, fmt.Errorf("create controller: %w", err)
	}

	eng := newScenarioEngine(ctx, nodeID, cluster, clock, deps, newController)

	mgr, err := supervisor.New(ctx, dataRoot,
		supervisor.WithSpecStore(deps.specStore),
		supervisor.WithManagerStateStore(deps.stateStore),
		supervisor.WithManagerController(ctrl),
		supervisor.WithManagerEngine(eng),
	)
	if err != nil {
		return nil, fmt.Errorf("create manager: %w", err)
	}

	return &Node{
		ID:               nodeID,
		DataRoot:         dataRoot,
		Manager:          mgr,
		PlatformOps:      deps.platformOps,
		StateStore:       deps.stateStore,
		SpecStore:        deps.specStore,
		ContainerRuntime: deps.containerRuntime,
		CorrosionRuntime: deps.corrosionRuntime,
		StatusProber:     deps.statusProber,
	}, nil
}

func newNodeDependencies() nodeDependencies {
	return nodeDependencies{
		stateStore:       leaf.NewStateStore(),
		specStore:        leaf.NewSpecStore(),
		platformOps:      &leaf.PlatformOps{},
		containerRuntime: leaf.NewContainerRuntime(),
		corrosionRuntime: leaf.NewCorrosionRuntime(),
		statusProber:     &leaf.StatusProber{WG: true, DockerNet: true, Corrosion: true},
	}
}

func newControllerFactory(nodeID string, cluster *cluster.Cluster, clock mesh.Clock, deps nodeDependencies) func(opts ...mesh.Option) (*mesh.Controller, error) {
	check.Assert(cluster != nil, "scenario.newControllerFactory: cluster must not be nil")
	check.Assert(clock != nil, "scenario.newControllerFactory: clock must not be nil")

	baseControllerOpts := []mesh.Option{
		mesh.WithStateStore(deps.stateStore),
		mesh.WithPlatformOps(deps.platformOps),
		mesh.WithContainerRuntime(deps.containerRuntime),
		mesh.WithCorrosionRuntime(deps.corrosionRuntime),
		mesh.WithStatusProber(deps.statusProber),
		mesh.WithRegistryFactory(cluster.NetworkRegistryFactory(nodeID)),
		mesh.WithClock(clock),
	}

	return func(opts ...mesh.Option) (*mesh.Controller, error) {
		allOpts := make([]mesh.Option, 0, len(baseControllerOpts)+len(opts))
		allOpts = append(allOpts, baseControllerOpts...)
		allOpts = append(allOpts, opts...)
		return mesh.New(allOpts...)
	}
}

func newScenarioEngine(
	ctx context.Context,
	nodeID string,
	cluster *cluster.Cluster,
	clock mesh.Clock,
	deps nodeDependencies,
	newController func(opts ...mesh.Option) (*mesh.Controller, error),
) *engine.Engine {
	healthyNTP := func() reconcile.NTPStatus {
		return reconcile.NTPStatus{Healthy: true}
	}

	return engine.New(ctx,
		engine.WithControllerFactory(func() (engine.NetworkController, error) {
			return newController()
		}),
		engine.WithPeerReconcilerFactory(func() (reconcile.PeerReconciler, error) {
			return newController()
		}),
		engine.WithRegistryFactory(cluster.ReconcileRegistryFactory(nodeID)),
		engine.WithStateStore(deps.stateStore),
		engine.WithClock(clock),
		engine.WithPingDialFunc(cluster.DialFunc(nodeID)),
		engine.WithNTPCheckFunc(healthyNTP),
	)
}
