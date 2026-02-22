package fake

import (
	"context"
	"fmt"
	"math/rand"
	"net/netip"
	"sort"
	"sync"
	"time"

	"ployz/internal/network"
)

// Cluster simulates a Corrosion gossip cluster with N nodes.
// All operations are deterministic — no goroutines, no real time.
type Cluster struct {
	mu        sync.RWMutex
	clock     *Clock
	nodes     map[string]*nodeState
	links     map[link]*LinkConfig
	blocked   map[link]bool
	pending   []pendingWrite
	rng       *rand.Rand
	nodeAddrs map[string]string // addr → nodeID, for DialFunc lookups
}

type nodeState struct {
	machines    map[string]network.MachineRow
	heartbeats  map[string]network.HeartbeatRow
	networkCIDR netip.Prefix
	machSubs    []chan<- network.MachineChange
	hbSubs      []chan<- network.HeartbeatChange
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
	target    string
	write     writeOp
}

type writeOp struct {
	kind      writeKind
	machine   network.MachineRow
	heartbeat network.HeartbeatRow
	deleteID  string
}

type writeKind int

const (
	writeUpsertMachine writeKind = iota
	writeDeleteMachine
	writeHeartbeat
)

// NodeSnapshot is a point-in-time view of a node's local data.
type NodeSnapshot struct {
	Machines   []network.MachineRow
	Heartbeats []network.HeartbeatRow
}

// Machine returns the machine row with the given ID, if present.
func (s NodeSnapshot) Machine(id string) (network.MachineRow, bool) {
	for _, m := range s.Machines {
		if m.ID == id {
			return m, true
		}
	}
	return network.MachineRow{}, false
}

// NewCluster creates a cluster backed by the given fake clock.
func NewCluster(clock *Clock) *Cluster {
	// TODO: random seed for Drop rate (currently uses math/rand)
	return &Cluster{
		clock:     clock,
		nodes:     make(map[string]*nodeState),
		links:     make(map[link]*LinkConfig),
		blocked:   make(map[link]bool),
		rng:       rand.New(rand.NewSource(42)),
		nodeAddrs: make(map[string]string),
	}
}

func (c *Cluster) ensureNode(id string) *nodeState {
	n, ok := c.nodes[id]
	if !ok {
		n = &nodeState{
			machines:   make(map[string]network.MachineRow),
			heartbeats: make(map[string]network.HeartbeatRow),
		}
		c.nodes[id] = n
	}
	return n
}

// Registry returns (or creates) a Registry view for the given node.
func (c *Cluster) Registry(nodeID string) *Registry {
	c.mu.Lock()
	c.ensureNode(nodeID)
	c.mu.Unlock()
	return NewRegistry(c, nodeID)
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

// Heal removes all partitions.
func (c *Cluster) Heal() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.blocked = make(map[link]bool)
}

// Tick delivers pending writes up to clock.Now().
func (c *Cluster) Tick() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.deliverUpTo(c.clock.Now())
}

// Drain delivers ALL pending writes regardless of time.
func (c *Cluster) Drain() {
	c.mu.Lock()
	defer c.mu.Unlock()

	for _, pw := range c.pending {
		c.applyWrite(pw.target, pw.write)
	}
	c.pending = nil
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

		// Find which node owns this address by checking machine endpoints.
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

// NetworkRegistryFactory returns a network.RegistryFactory for the given node.
func (c *Cluster) NetworkRegistryFactory(nodeID string) network.RegistryFactory {
	return func(addr netip.AddrPort, token string) network.Registry {
		return c.Registry(nodeID)
	}
}

// ReconcileRegistryFactory returns an engine.RegistryFactory for the given node.
// It returns the same *Registry cast to reconcile.Registry.
func (c *Cluster) ReconcileRegistryFactory(nodeID string) func(addr netip.AddrPort, token string) *Registry {
	return func(addr netip.AddrPort, token string) *Registry {
		return c.Registry(nodeID)
	}
}

// Internal: upsert a machine from the perspective of writerNode.
func (c *Cluster) upsertMachine(writerNode string, row network.MachineRow) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	n := c.ensureNode(writerNode)

	// Optimistic concurrency: check version if expectedVersion set in row.
	if existing, ok := n.machines[row.ID]; ok {
		// Version is bumped by the caller. Check happens before write.
		if row.Version > 0 && existing.Version != row.Version-1 {
			return network.ErrConflict
		}
	}

	n.machines[row.ID] = row

	// Notify local subscribers.
	change := network.MachineChange{Kind: network.ChangeUpdated, Machine: row}
	for _, ch := range n.machSubs {
		select {
		case ch <- change:
		default:
		}
	}

	// Fan out to other nodes.
	op := writeOp{kind: writeUpsertMachine, machine: row}
	c.fanOut(writerNode, op)
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

	change := network.MachineChange{Kind: network.ChangeDeleted, Machine: row}
	for _, ch := range n.machSubs {
		select {
		case ch <- change:
		default:
		}
	}

	op := writeOp{kind: writeDeleteMachine, deleteID: machineID, machine: row}
	c.fanOut(writerNode, op)
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

		change := network.MachineChange{Kind: network.ChangeDeleted, Machine: row}
		for _, ch := range n.machSubs {
			select {
			case ch <- change:
			default:
			}
		}

		op := writeOp{kind: writeDeleteMachine, deleteID: id, machine: row}
		c.fanOut(writerNode, op)
	}
}

// Internal: bump heartbeat from the perspective of writerNode.
func (c *Cluster) bumpHeartbeat(writerNode string, nodeID string, updatedAt string) {
	c.mu.Lock()
	defer c.mu.Unlock()

	n := c.ensureNode(writerNode)
	existing := n.heartbeats[nodeID]
	hb := network.HeartbeatRow{
		NodeID:    nodeID,
		Seq:       existing.Seq + 1,
		UpdatedAt: updatedAt,
	}
	n.heartbeats[nodeID] = hb

	change := network.HeartbeatChange{Kind: network.ChangeUpdated, Heartbeat: hb}
	for _, ch := range n.hbSubs {
		select {
		case ch <- change:
		default:
		}
	}

	op := writeOp{kind: writeHeartbeat, heartbeat: hb}
	c.fanOut(writerNode, op)
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
func (c *Cluster) listMachines(nodeID string) []network.MachineRow {
	c.mu.RLock()
	defer c.mu.RUnlock()

	n, ok := c.nodes[nodeID]
	if !ok {
		return nil
	}
	return sortedMachines(n.machines)
}

// Internal: list heartbeats for a specific node's local view.
func (c *Cluster) listHeartbeats(nodeID string) []network.HeartbeatRow {
	c.mu.RLock()
	defer c.mu.RUnlock()

	n, ok := c.nodes[nodeID]
	if !ok {
		return nil
	}
	return sortedHeartbeats(n.heartbeats)
}

// Internal: subscribe to machine changes on a specific node.
func (c *Cluster) subscribeMachines(ctx context.Context, nodeID string) ([]network.MachineRow, <-chan network.MachineChange, error) {
	c.mu.Lock()
	n := c.ensureNode(nodeID)

	snapshot := sortedMachines(n.machines)
	// TODO: configurable subscription buffer size
	ch := make(chan network.MachineChange, 256)
	n.machSubs = append(n.machSubs, ch)
	c.mu.Unlock()

	go func() {
		<-ctx.Done()
		c.mu.Lock()
		defer c.mu.Unlock()
		subs := c.nodes[nodeID].machSubs
		for i, s := range subs {
			if s == ch {
				c.nodes[nodeID].machSubs = append(subs[:i], subs[i+1:]...)
				break
			}
		}
		close(ch)
	}()

	return snapshot, ch, nil
}

// Internal: subscribe to heartbeat changes on a specific node.
func (c *Cluster) subscribeHeartbeats(ctx context.Context, nodeID string) ([]network.HeartbeatRow, <-chan network.HeartbeatChange, error) {
	c.mu.Lock()
	n := c.ensureNode(nodeID)

	snapshot := sortedHeartbeats(n.heartbeats)
	ch := make(chan network.HeartbeatChange, 256)
	n.hbSubs = append(n.hbSubs, ch)
	c.mu.Unlock()

	go func() {
		<-ctx.Done()
		c.mu.Lock()
		defer c.mu.Unlock()
		subs := c.nodes[nodeID].hbSubs
		for i, s := range subs {
			if s == ch {
				c.nodes[nodeID].hbSubs = append(subs[:i], subs[i+1:]...)
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
	for nodeID := range c.nodes {
		if nodeID == writerNode {
			continue
		}
		l := link{writerNode, nodeID}

		if c.blocked[l] {
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
		change := network.MachineChange{Kind: network.ChangeUpdated, Machine: op.machine}
		for _, ch := range n.machSubs {
			select {
			case ch <- change:
			default:
			}
		}

	case writeDeleteMachine:
		delete(n.machines, op.deleteID)
		change := network.MachineChange{Kind: network.ChangeDeleted, Machine: op.machine}
		for _, ch := range n.machSubs {
			select {
			case ch <- change:
			default:
			}
		}

	case writeHeartbeat:
		n.heartbeats[op.heartbeat.NodeID] = op.heartbeat
		change := network.HeartbeatChange{Kind: network.ChangeUpdated, Heartbeat: op.heartbeat}
		for _, ch := range n.hbSubs {
			select {
			case ch <- change:
			default:
			}
		}
	}
}

// deliverUpTo processes pending writes up to the given time.
// Must be called with c.mu held.
func (c *Cluster) deliverUpTo(t time.Time) {
	var remaining []pendingWrite
	for _, pw := range c.pending {
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

func sortedMachines(m map[string]network.MachineRow) []network.MachineRow {
	out := make([]network.MachineRow, 0, len(m))
	for _, row := range m {
		out = append(out, row)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out
}

func sortedHeartbeats(m map[string]network.HeartbeatRow) []network.HeartbeatRow {
	out := make([]network.HeartbeatRow, 0, len(m))
	for _, row := range m {
		out = append(out, row)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].NodeID < out[j].NodeID })
	return out
}
