package fake

import (
	"cmp"
	"context"
	"crypto/rand"
	"encoding/binary"
	"errors"
	"fmt"
	mathrand "math/rand"
	"net/netip"
	"slices"
	"sync"
	"time"

	"ployz/internal/check"
	"ployz/internal/mesh"
	"ployz/internal/reconcile"
)

// ClusterOption configures optional Cluster behavior.
type ClusterOption func(*clusterConfig)

type clusterConfig struct {
	seed    int64
	seedSet bool
}

// WithRandSeed sets a deterministic seed for the RNG used in drop-rate
// decisions. If not provided, the seed is derived from crypto/rand.
func WithRandSeed(seed int64) ClusterOption {
	return func(cfg *clusterConfig) {
		cfg.seed = seed
		cfg.seedSet = true
	}
}

// ErrNodeDead is returned by Registry methods when the node has been killed.
var ErrNodeDead = errors.New("node is dead")

// Cluster simulates a Corrosion gossip cluster with N nodes.
// All operations are deterministic — no goroutines, no real time.
type Cluster struct {
	mu         sync.RWMutex
	clock      mesh.Clock
	nodes      map[string]*nodeState
	registries map[string]*Registry             // cached per-node registries
	links      map[link]*LinkConfig
	blocked    map[link]bool
	killed     map[string]bool
	pending    []pendingWrite
	rng        *mathrand.Rand
	nodeAddrs  map[string]string                // addr → nodeID, for DialFunc lookups
	linkPred   func(from, to string) bool       // if non-nil, consulted before delivery
}

type nodeState struct {
	machines    map[string]mesh.MachineRow
	heartbeats  map[string]mesh.HeartbeatRow
	networkCIDR netip.Prefix
	machineSubscriptions   []chan<- mesh.MachineChange
	heartbeatSubscriptions []chan<- mesh.HeartbeatChange
}

// LinkConfig controls replication behavior between two nodes.
type LinkConfig struct {
	Latency time.Duration // replication delay (0 = instant)
	PingRTT time.Duration // simulated ping RTT (independent of replication)
	Drop    float64       // 0.0–1.0 random loss rate
	Err     func() error  // hard replication failure
}

type link struct{ from, to string }

type pendingWrite struct {
	deliverAt time.Time
	from      string
	target    string
	write     writeOp
}

type writeOp struct {
	kind      writeKind
	machine   mesh.MachineRow
	heartbeat mesh.HeartbeatRow
	deleteID  string
}

type writeKind int

const (
	writeUpsertMachine writeKind = iota
	writeDeleteMachine
	writeHeartbeat

	// subscriptionBufCapacity is 256: sized to absorb burst from full cluster anti-entropy.
	subscriptionBufCapacity = 256
)

// NodeSnapshot is a point-in-time view of a node's local data.
type NodeSnapshot struct {
	Machines   []mesh.MachineRow
	Heartbeats []mesh.HeartbeatRow
}

// Machine returns the machine row with the given ID, if present.
func (s NodeSnapshot) Machine(id string) (mesh.MachineRow, bool) {
	for _, m := range s.Machines {
		if m.ID == id {
			return m, true
		}
	}
	return mesh.MachineRow{}, false
}

// NewCluster creates a cluster backed by the given fake clock.
// Options can configure deterministic seeding for drop-rate RNG via WithRandSeed.
func NewCluster(clock mesh.Clock, opts ...ClusterOption) *Cluster {
	check.Assert(clock != nil, "NewCluster: clock must not be nil")

	var cfg clusterConfig
	for _, o := range opts {
		o(&cfg)
	}

	seed := cfg.seed
	if !cfg.seedSet {
		var buf [8]byte
		_, _ = rand.Read(buf[:])
		seed = int64(binary.LittleEndian.Uint64(buf[:]))
	}

	return &Cluster{
		clock:      clock,
		nodes:      make(map[string]*nodeState),
		registries: make(map[string]*Registry),
		links:      make(map[link]*LinkConfig),
		blocked:    make(map[link]bool),
		killed:     make(map[string]bool),
		rng:        mathrand.New(mathrand.NewSource(seed)),
		nodeAddrs:  make(map[string]string),
	}
}

func (c *Cluster) ensureNode(id string) *nodeState {
	n, ok := c.nodes[id]
	if !ok {
		n = &nodeState{
			machines:   make(map[string]mesh.MachineRow),
			heartbeats: make(map[string]mesh.HeartbeatRow),
		}
		c.nodes[id] = n
	}
	return n
}

// Registry returns the cached Registry for the given node, creating it on first call.
// The same *Registry is returned for the same nodeID, so error injection hooks
// set on the returned value affect all components using this node's registry.
func (c *Cluster) Registry(nodeID string) *Registry {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.ensureNode(nodeID)
	if r, ok := c.registries[nodeID]; ok {
		return r
	}
	r := NewRegistry(c, nodeID)
	c.registries[nodeID] = r
	return r
}

// RegisterAddr maps an address (e.g. "host:port") to a node ID for DialFunc lookups.
func (c *Cluster) RegisterAddr(nodeID, addr string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.nodeAddrs[addr] = nodeID
}

// SetLink configures the replication link from → to.
func (c *Cluster) SetLink(from, to string, cfg LinkConfig) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.links[link{from, to}] = &cfg
}

// BlockLink blocks replication from → to (asymmetric partition).
func (c *Cluster) BlockLink(from, to string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.blocked[link{from, to}] = true
}

// Partition creates a bidirectional partition between two groups.
func (c *Cluster) Partition(groupA, groupB []string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, a := range groupA {
		for _, b := range groupB {
			c.blocked[link{a, b}] = true
			c.blocked[link{b, a}] = true
		}
	}
}

// SetLinkPredicate installs a callback consulted before delivering writes.
// If pred returns false for a (from, to) pair, the write stays in pending
// and is re-checked on every Tick/Drain (modeling gossip retry to unreachable peers).
// Pass nil to remove the predicate.
func (c *Cluster) SetLinkPredicate(pred func(from, to string) bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.linkPred = pred
}

// Heal removes all partitions.
func (c *Cluster) Heal() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.blocked = make(map[link]bool)
}

// KillNode marks a node as dead. Registry ops return ErrNodeDead,
// replication to/from is blocked, subscription channels go silent.
// Local state is preserved (simulates SQLite surviving crash).
func (c *Cluster) KillNode(nodeID string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.killed[nodeID] = true
}

// RestartNode marks a killed node as alive and runs anti-entropy sync
// from reachable peers. Consumers must re-subscribe after restart.
func (c *Cluster) RestartNode(nodeID string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	delete(c.killed, nodeID)
	c.antiEntropy(nodeID)
}

// IsKilled reports whether a node is currently killed.
func (c *Cluster) IsKilled(nodeID string) bool {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.killed[nodeID]
}

// antiEntropy merges state from all reachable, non-killed, non-blocked peers
// into the given node. For machines: highest version wins. For heartbeats:
// highest seq wins. Machines that exist locally but on zero reachable peers
// are deleted (unless no peers are reachable at all).
// Must be called with c.mu held.
func (c *Cluster) antiEntropy(nodeID string) {
	n, ok := c.nodes[nodeID]
	if !ok {
		return
	}

	// Collect reachable peers.
	var reachable []*nodeState
	for peerID, peerState := range c.nodes {
		if peerID == nodeID {
			continue
		}
		if c.killed[peerID] {
			continue
		}
		if c.blocked[link{peerID, nodeID}] || c.blocked[link{nodeID, peerID}] {
			continue
		}
		reachable = append(reachable, peerState)
	}

	if len(reachable) == 0 {
		return
	}

	// Merge machines: highest version wins.
	for _, peer := range reachable {
		for id, peerRow := range peer.machines {
			local, exists := n.machines[id]
			if !exists || peerRow.Version > local.Version {
				n.machines[id] = peerRow
			}
		}
	}

	// Delete machines that exist locally but on zero reachable peers.
	for id := range n.machines {
		found := false
		for _, peer := range reachable {
			if _, ok := peer.machines[id]; ok {
				found = true
				break
			}
		}
		if !found {
			delete(n.machines, id)
		}
	}

	// Merge heartbeats: highest seq wins.
	for _, peer := range reachable {
		for id, peerHB := range peer.heartbeats {
			local, exists := n.heartbeats[id]
			if !exists || peerHB.Seq > local.Seq {
				n.heartbeats[id] = peerHB
			}
		}
	}
}

// Tick delivers pending writes up to clock.Now().
func (c *Cluster) Tick() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.deliverUpTo(c.clock.Now())
}

// Drain delivers ALL pending writes regardless of time.
// Writes targeting killed nodes are retained.
func (c *Cluster) Drain() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.deliverUpTo(time.Date(9999, 12, 31, 23, 59, 59, 0, time.UTC))
}

// Snapshot returns a point-in-time view of a node's local data.
func (c *Cluster) Snapshot(nodeID string) NodeSnapshot {
	c.mu.RLock()
	defer c.mu.RUnlock()

	n, ok := c.nodes[nodeID]
	if !ok {
		return NodeSnapshot{}
	}
	return NodeSnapshot{
		Machines:   sortedMachines(n.machines),
		Heartbeats: sortedHeartbeats(n.heartbeats),
	}
}

// PingRTT returns the configured RTT for the link from → to.
func (c *Cluster) PingRTT(from, to string) time.Duration {
	c.mu.RLock()
	defer c.mu.RUnlock()

	if c.blocked[link{from, to}] {
		return -1
	}
	lc := c.links[link{from, to}]
	if lc == nil {
		return 0
	}
	return lc.PingRTT
}

// DialFunc returns a function suitable for PingTracker.DialFunc that uses
// configured PingRTT values instead of real TCP dials.
func (c *Cluster) DialFunc(fromNodeID string) func(ctx context.Context, addr string) (time.Duration, error) {
	return func(ctx context.Context, addr string) (time.Duration, error) {
		c.mu.RLock()
		defer c.mu.RUnlock()

		targetNode := c.nodeByAddr(addr)
		if targetNode == "" {
			return -1, fmt.Errorf("no node found for address %s", addr)
		}

		if c.blocked[link{fromNodeID, targetNode}] {
			return -1, fmt.Errorf("link %s→%s blocked", fromNodeID, targetNode)
		}

		lc := c.links[link{fromNodeID, targetNode}]
		if lc == nil {
			return time.Millisecond, nil // default: 1ms
		}
		if lc.PingRTT <= 0 {
			return time.Millisecond, nil
		}
		return lc.PingRTT, nil
	}
}

// NetworkRegistryFactory returns a mesh.RegistryFactory for the given node.
func (c *Cluster) NetworkRegistryFactory(nodeID string) mesh.RegistryFactory {
	return func(addr netip.AddrPort, token string) mesh.Registry {
		return c.Registry(nodeID)
	}
}

// ReconcileRegistryFactory returns a factory for the given node that produces
// reconcile.Registry values. Used to wire Workers in test harnesses.
func (c *Cluster) ReconcileRegistryFactory(nodeID string) func(addr netip.AddrPort, token string) reconcile.Registry {
	return func(addr netip.AddrPort, token string) reconcile.Registry {
		return c.Registry(nodeID)
	}
}

// Internal: upsert a machine from the perspective of writerNode.
// Matches real Corrosion semantics: expectedVersion=0 means unconditional write,
// expectedVersion>0 requires the existing row's version to match exactly.
func (c *Cluster) upsertMachine(writerNode string, row mesh.MachineRow, expectedVersion int64) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	n := c.ensureNode(writerNode)

	if existing, ok := n.machines[row.ID]; ok {
		if expectedVersion > 0 && existing.Version != expectedVersion {
			return mesh.ErrConflict
		}
		row.Version = existing.Version + 1
	} else {
		if expectedVersion > 0 {
			return mesh.ErrConflict
		}
		if row.Version <= 0 {
			row.Version = 1
		}
	}

	n.machines[row.ID] = row
	notifyMachineSubs(n, mesh.MachineChange{Kind: mesh.ChangeUpdated, Machine: row})

	c.fanOut(writerNode, writeOp{kind: writeUpsertMachine, machine: row})
	return nil
}

// Internal: delete a machine from the perspective of writerNode.
func (c *Cluster) deleteMachine(writerNode string, machineID string) {
	c.mu.Lock()
	defer c.mu.Unlock()

	n := c.ensureNode(writerNode)
	row, ok := n.machines[machineID]
	if !ok {
		return
	}
	delete(n.machines, machineID)
	notifyMachineSubs(n, mesh.MachineChange{Kind: mesh.ChangeDeleted, Machine: row})

	c.fanOut(writerNode, writeOp{kind: writeDeleteMachine, deleteID: machineID, machine: row})
}

// Internal: delete machines by endpoint except a specific ID.
func (c *Cluster) deleteByEndpointExceptID(writerNode string, endpoint string, exceptID string) {
	c.mu.Lock()
	defer c.mu.Unlock()

	n := c.ensureNode(writerNode)
	var toDelete []string
	for id, m := range n.machines {
		if m.Endpoint == endpoint && id != exceptID {
			toDelete = append(toDelete, id)
		}
	}

	for _, id := range toDelete {
		row := n.machines[id]
		delete(n.machines, id)
		notifyMachineSubs(n, mesh.MachineChange{Kind: mesh.ChangeDeleted, Machine: row})
		c.fanOut(writerNode, writeOp{kind: writeDeleteMachine, deleteID: id, machine: row})
	}
}

// Internal: bump heartbeat from the perspective of writerNode.
func (c *Cluster) bumpHeartbeat(writerNode string, nodeID string, updatedAt string) {
	c.mu.Lock()
	defer c.mu.Unlock()

	n := c.ensureNode(writerNode)
	existing := n.heartbeats[nodeID]
	hb := mesh.HeartbeatRow{
		NodeID:    nodeID,
		Seq:       existing.Seq + 1,
		UpdatedAt: updatedAt,
	}
	n.heartbeats[nodeID] = hb
	notifyHeartbeatSubs(n, mesh.HeartbeatChange{Kind: mesh.ChangeUpdated, Heartbeat: hb})

	c.fanOut(writerNode, writeOp{kind: writeHeartbeat, heartbeat: hb})
}

// Internal: ensure network CIDR with first-writer-wins semantics.
func (c *Cluster) ensureNetworkCIDR(writerNode string, requested netip.Prefix, fallbackCIDR string, defaultCIDR netip.Prefix) (netip.Prefix, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	// Check if any node already has a CIDR set.
	for _, n := range c.nodes {
		if n.networkCIDR.IsValid() {
			return n.networkCIDR, nil
		}
	}

	// First writer wins: determine what CIDR to use.
	cidr := requested
	if !cidr.IsValid() && fallbackCIDR != "" {
		parsed, err := netip.ParsePrefix(fallbackCIDR)
		if err == nil {
			cidr = parsed
		}
	}
	if !cidr.IsValid() {
		cidr = defaultCIDR
	}

	// Set on all nodes.
	for _, n := range c.nodes {
		n.networkCIDR = cidr
	}
	return cidr, nil
}

// Internal: list machines for a specific node's local view.
func (c *Cluster) listMachines(nodeID string) []mesh.MachineRow {
	c.mu.RLock()
	defer c.mu.RUnlock()

	n, ok := c.nodes[nodeID]
	if !ok {
		return nil
	}
	return sortedMachines(n.machines)
}

// Internal: subscribe to machine changes on a specific node.
func (c *Cluster) subscribeMachines(ctx context.Context, nodeID string) ([]mesh.MachineRow, <-chan mesh.MachineChange, error) {
	c.mu.Lock()
	n := c.ensureNode(nodeID)

	snapshot := sortedMachines(n.machines)
	ch := make(chan mesh.MachineChange, subscriptionBufCapacity)
	n.machineSubscriptions = append(n.machineSubscriptions, ch)
	c.mu.Unlock()

	go func() {
		<-ctx.Done()
		c.mu.Lock()
		defer c.mu.Unlock()
		subs := c.nodes[nodeID].machineSubscriptions
		for i, s := range subs {
			if s == ch {
				c.nodes[nodeID].machineSubscriptions = append(subs[:i], subs[i+1:]...)
				break
			}
		}
		close(ch)
	}()

	return snapshot, ch, nil
}

// Internal: subscribe to heartbeat changes on a specific node.
func (c *Cluster) subscribeHeartbeats(ctx context.Context, nodeID string) ([]mesh.HeartbeatRow, <-chan mesh.HeartbeatChange, error) {
	c.mu.Lock()
	n := c.ensureNode(nodeID)

	snapshot := sortedHeartbeats(n.heartbeats)
	ch := make(chan mesh.HeartbeatChange, subscriptionBufCapacity)
	n.heartbeatSubscriptions = append(n.heartbeatSubscriptions, ch)
	c.mu.Unlock()

	go func() {
		<-ctx.Done()
		c.mu.Lock()
		defer c.mu.Unlock()
		subs := c.nodes[nodeID].heartbeatSubscriptions
		for i, s := range subs {
			if s == ch {
				c.nodes[nodeID].heartbeatSubscriptions = append(subs[:i], subs[i+1:]...)
				break
			}
		}
		close(ch)
	}()

	return snapshot, ch, nil
}

// fanOut replicates a write to all nodes except the writer.
// Must be called with c.mu held.
func (c *Cluster) fanOut(writerNode string, op writeOp) {
	if c.killed[writerNode] {
		return
	}
	for nodeID := range c.nodes {
		if nodeID == writerNode {
			continue
		}
		if c.killed[nodeID] {
			continue
		}
		l := link{writerNode, nodeID}

		if c.blocked[l] {
			continue
		}

		// LinkPredicate: if set and returns false, enqueue as pending for retry.
		if c.linkPred != nil && !c.linkPred(writerNode, nodeID) {
			c.pending = append(c.pending, pendingWrite{
				deliverAt: c.clock.Now(),
				from:      writerNode,
				target:    nodeID,
				write:     op,
			})
			continue
		}

		lc := c.links[l]
		if lc != nil {
			if lc.Err != nil {
				if err := lc.Err(); err != nil {
					continue
				}
			}
			if lc.Drop > 0 && c.rng.Float64() < lc.Drop {
				continue
			}
			if lc.Latency > 0 {
				c.pending = append(c.pending, pendingWrite{
					deliverAt: c.clock.Now().Add(lc.Latency),
					from:      writerNode,
					target:    nodeID,
					write:     op,
				})
				continue
			}
		}

		// Instant delivery.
		c.applyWrite(nodeID, op)
	}
}

// applyWrite applies a write operation to the target node.
// Must be called with c.mu held.
func (c *Cluster) applyWrite(target string, op writeOp) {
	n := c.ensureNode(target)

	switch op.kind {
	case writeUpsertMachine:
		n.machines[op.machine.ID] = op.machine
		notifyMachineSubs(n, mesh.MachineChange{Kind: mesh.ChangeUpdated, Machine: op.machine})

	case writeDeleteMachine:
		delete(n.machines, op.deleteID)
		notifyMachineSubs(n, mesh.MachineChange{Kind: mesh.ChangeDeleted, Machine: op.machine})

	case writeHeartbeat:
		n.heartbeats[op.heartbeat.NodeID] = op.heartbeat
		notifyHeartbeatSubs(n, mesh.HeartbeatChange{Kind: mesh.ChangeUpdated, Heartbeat: op.heartbeat})

	default:
		panic(fmt.Sprintf("unknown writeKind: %d", op.kind))
	}
}

// deliverUpTo processes pending writes up to the given time.
// Must be called with c.mu held.
func (c *Cluster) deliverUpTo(t time.Time) {
	var remaining []pendingWrite
	for _, pw := range c.pending {
		if c.killed[pw.target] {
			remaining = append(remaining, pw)
			continue
		}
		if c.linkPred != nil && pw.from != "" && !c.linkPred(pw.from, pw.target) {
			remaining = append(remaining, pw)
			continue
		}
		if !pw.deliverAt.After(t) {
			c.applyWrite(pw.target, pw.write)
		} else {
			remaining = append(remaining, pw)
		}
	}
	c.pending = remaining
}

// nodeByAddr finds which node owns a given address.
// Uses the explicit RegisterAddr mapping first, then falls back to machine endpoints.
// Must be called with c.mu held (at least RLock).
func (c *Cluster) nodeByAddr(addr string) string {
	if nodeID, ok := c.nodeAddrs[addr]; ok {
		return nodeID
	}
	for nodeID, n := range c.nodes {
		for _, m := range n.machines {
			if m.Endpoint == addr {
				return nodeID
			}
		}
	}
	return ""
}

// notifyMachineSubs sends a change to all machine subscription channels on the node.
// Drops the change if a channel's buffer is full. Must be called with c.mu held.
func notifyMachineSubs(n *nodeState, change mesh.MachineChange) {
	for _, ch := range n.machineSubscriptions {
		select {
		case ch <- change:
		default:
		}
	}
}

// notifyHeartbeatSubs sends a change to all heartbeat subscription channels on the node.
// Drops the change if a channel's buffer is full. Must be called with c.mu held.
func notifyHeartbeatSubs(n *nodeState, change mesh.HeartbeatChange) {
	for _, ch := range n.heartbeatSubscriptions {
		select {
		case ch <- change:
		default:
		}
	}
}

func sortedMachines(m map[string]mesh.MachineRow) []mesh.MachineRow {
	out := make([]mesh.MachineRow, 0, len(m))
	for _, row := range m {
		out = append(out, row)
	}
	slices.SortFunc(out, func(a, b mesh.MachineRow) int { return cmp.Compare(a.ID, b.ID) })
	return out
}

func sortedHeartbeats(m map[string]mesh.HeartbeatRow) []mesh.HeartbeatRow {
	out := make([]mesh.HeartbeatRow, 0, len(m))
	for _, row := range m {
		out = append(out, row)
	}
	slices.SortFunc(out, func(a, b mesh.HeartbeatRow) int { return cmp.Compare(a.NodeID, b.NodeID) })
	return out
}
