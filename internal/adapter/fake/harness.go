package fake

import (
	"context"
	"fmt"
	"net/netip"
	"sync"

	"ployz/internal/check"
	"ployz/internal/mesh"
	"ployz/internal/reconcile"
)

// Harness spins up N reconcile.Worker instances sharing a single Cluster.
// Designed for use inside synctest.Test where time.Now() returns fake time.
type Harness struct {
	Cluster *Cluster
	nodes   map[string]*HarnessNode
	network string
	cancel  context.CancelFunc
	wg      sync.WaitGroup
}

// HarnessNode holds per-node state for a harness-managed worker.
type HarnessNode struct {
	ID         string
	Registry   *Registry
	Reconciler *PeerReconciler
	Store      *StateStore

	mu        sync.Mutex
	workerErr error // set when worker exits
}

// Err returns the error the worker exited with, if any.
func (n *HarnessNode) Err() error {
	n.mu.Lock()
	defer n.mu.Unlock()
	return n.workerErr
}

// HarnessConfig configures a Harness.
type HarnessConfig struct {
	NodeIDs []string
	Network string // default: "chaos"
}

// NewHarness creates a harness with N nodes, each with its own Registry,
// PeerReconciler, and StateStore. The Cluster uses mesh.RealClock{} so that
// inside synctest, time.Now() returns fake time for both Cluster and Workers.
func NewHarness(cfg HarnessConfig) *Harness {
	check.Assert(len(cfg.NodeIDs) > 0, "NewHarness: NodeIDs must not be empty")
	if cfg.Network == "" {
		cfg.Network = "chaos"
	}

	cluster := NewCluster(mesh.RealClock{})
	nodes := make(map[string]*HarnessNode, len(cfg.NodeIDs))

	// Phase 1: Create all nodes, registries, and state stores.
	// Nodes must all exist before upserting machines so fan-out reaches everyone.
	type nodeSetup struct {
		nodeID string
		subnet string
		hostIP string
		reg    *Registry
	}
	setups := make([]nodeSetup, len(cfg.NodeIDs))

	for i, nodeID := range cfg.NodeIDs {
		reg := cluster.Registry(nodeID)
		pr := NewPeerReconciler()
		store := NewStateStore()

		subnetIdx := i + 1
		subnet := fmt.Sprintf("10.210.%d.0/24", subnetIdx)

		dataDir := fmt.Sprintf("%s/%s", nodeID, cfg.Network)
		_ = store.Save(dataDir, &mesh.State{
			Network:   cfg.Network,
			Subnet:    subnet,
			WGPublic:  nodeID,
			WGPrivate: "fake-private-" + nodeID,
		})

		prefix := netip.MustParsePrefix(subnet)
		hostIP := mesh.MachineIP(prefix)
		cluster.RegisterAddr(nodeID, hostIP.String())

		setups[i] = nodeSetup{nodeID: nodeID, subnet: subnet, hostIP: hostIP.String(), reg: reg}
		nodes[nodeID] = &HarnessNode{
			ID:         nodeID,
			Registry:   reg,
			Reconciler: pr,
			Store:      store,
		}
	}

	// Phase 2: Upsert machine rows now that all nodes exist in the cluster.
	ctx := context.Background()
	for _, s := range setups {
		row := mesh.MachineRow{
			ID:        s.nodeID,
			PublicKey: s.nodeID,
			Subnet:    s.subnet,
			Endpoint:  fmt.Sprintf("%s:51820", s.hostIP),
		}
		_ = s.reg.UpsertMachine(ctx, row, 0)
	}

	return &Harness{
		Cluster: cluster,
		nodes:   nodes,
		network: cfg.Network,
	}
}

// Start launches a reconcile.Worker for each node. Workers run until Stop()
// or the parent context is cancelled.
func (h *Harness) Start(ctx context.Context) {
	check.Assert(h.cancel == nil, "Harness.Start: already started")
	ctx, h.cancel = context.WithCancel(ctx)

	for _, node := range h.nodes {
		w := &reconcile.Worker{
			Spec: mesh.Config{
				Network:  h.network,
				DataRoot: node.ID, // DataDir = <DataRoot>/<Network>
			},
			Registry:       node.Registry,
			PeerReconciler: node.Reconciler,
			StateStore:     node.Store,
			// Clock: nil → defaults to mesh.RealClock{} (fake time inside synctest)
			// NTP: nil → skip (makes real UDP calls)
			// Ping: nil → skip for basic tests
			// Freshness: nil → skip for basic tests
		}

		h.wg.Add(1)
		go func() {
			defer h.wg.Done()
			err := w.Run(ctx)
			node.mu.Lock()
			node.workerErr = err
			node.mu.Unlock()
		}()
	}
}

// Stop cancels all workers and waits for them to exit.
func (h *Harness) Stop() {
	if h.cancel != nil {
		h.cancel()
	}
	h.wg.Wait()
}

// Node returns the HarnessNode for the given ID.
func (h *Harness) Node(id string) *HarnessNode {
	return h.nodes[id]
}

// Nodes returns all node IDs.
func (h *Harness) Nodes() []string {
	ids := make([]string, 0, len(h.nodes))
	for id := range h.nodes {
		ids = append(ids, id)
	}
	return ids
}
