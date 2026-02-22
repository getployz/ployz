package fake

import (
	"context"
	"errors"
	"testing"
	"time"

	"ployz/internal/mesh"
)

func TestCluster_TwoNodesShare(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	ctx := context.Background()
	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1", Endpoint: "1.2.3.4:51820", Version: 0}
	if err := regA.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}

	// Node B should see the machine (instant replication, no link config).
	rows, err := regB.ListMachineRows(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || rows[0].ID != "m1" {
		t.Errorf("expected node-b to see m1, got %v", rows)
	}
}

func TestCluster_Subscription(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	_, ch, err := regB.SubscribeMachines(ctx)
	if err != nil {
		t.Fatal(err)
	}

	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1", Endpoint: "1.2.3.4:51820"}
	if err := regA.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}

	select {
	case change := <-ch:
		if change.Machine.ID != "m1" {
			t.Errorf("expected change for m1, got %q", change.Machine.ID)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for machine change")
	}
}

func TestCluster_Partition(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	c.Partition([]string{"node-a"}, []string{"node-b"})

	ctx := context.Background()
	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1"}
	if err := regA.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}

	// Node B should NOT see the machine.
	rows, _ := regB.ListMachineRows(ctx)
	if len(rows) != 0 {
		t.Errorf("expected node-b to see 0 machines during partition, got %d", len(rows))
	}

	// Node A should see its own write.
	rows, _ = regA.ListMachineRows(ctx)
	if len(rows) != 1 {
		t.Errorf("expected node-a to see 1 machine, got %d", len(rows))
	}
}

func TestCluster_HealAndDrain(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	// Set latency so writes queue up.
	c.SetLink("node-a", "node-b", LinkConfig{Latency: 200 * time.Millisecond})

	ctx := context.Background()
	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1"}
	if err := regA.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}

	// Before tick, B doesn't see it.
	rows, _ := regB.ListMachineRows(ctx)
	if len(rows) != 0 {
		t.Errorf("expected 0 machines on B before tick, got %d", len(rows))
	}

	// Drain delivers everything.
	c.Drain()
	rows, _ = regB.ListMachineRows(ctx)
	if len(rows) != 1 {
		t.Errorf("expected 1 machine on B after drain, got %d", len(rows))
	}
}

func TestCluster_Latency(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	c.SetLink("node-a", "node-b", LinkConfig{Latency: 200 * time.Millisecond})

	ctx := context.Background()
	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1"}
	_ = regA.UpsertMachine(ctx, row, 0)

	// Tick at +100ms — too early.
	clock.Advance(100 * time.Millisecond)
	c.Tick()
	rows, _ := regB.ListMachineRows(ctx)
	if len(rows) != 0 {
		t.Errorf("expected 0 machines at +100ms, got %d", len(rows))
	}

	// Tick at +250ms — delivered.
	clock.Advance(150 * time.Millisecond)
	c.Tick()
	rows, _ = regB.ListMachineRows(ctx)
	if len(rows) != 1 {
		t.Errorf("expected 1 machine at +250ms, got %d", len(rows))
	}
}

func TestCluster_AsymmetricBlock(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	c.BlockLink("node-a", "node-b") // A→B blocked, B→A open

	ctx := context.Background()

	// A writes — B should NOT see it.
	rowA := mesh.MachineRow{ID: "m1", PublicKey: "pk1"}
	_ = regA.UpsertMachine(ctx, rowA, 0)
	rows, _ := regB.ListMachineRows(ctx)
	if len(rows) != 0 {
		t.Errorf("expected B to not see A's write, got %d", len(rows))
	}

	// B writes — A SHOULD see it.
	rowB := mesh.MachineRow{ID: "m2", PublicKey: "pk2"}
	_ = regB.UpsertMachine(ctx, rowB, 0)
	rows, _ = regA.ListMachineRows(ctx)
	found := false
	for _, r := range rows {
		if r.ID == "m2" {
			found = true
		}
	}
	if !found {
		t.Error("expected A to see B's write (B→A not blocked)")
	}
}

func TestCluster_OptimisticConcurrency(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	reg := c.Registry("node-a")
	ctx := context.Background()

	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1"}
	if err := reg.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}

	// Version is now 1. Trying to upsert with expectedVersion=0 again should conflict.
	// UpsertMachine sets row.Version = expectedVersion+1, then cluster checks existing.Version != row.Version-1.
	// existing.Version = 1, row.Version = expectedVersion+1 = 1, row.Version-1 = 0, existing != 0 → conflict.
	err := reg.UpsertMachine(ctx, row, 0)
	if err != mesh.ErrConflict {
		t.Errorf("expected ErrConflict, got %v", err)
	}

	// Correct version should work.
	if err := reg.UpsertMachine(ctx, row, 1); err != nil {
		t.Errorf("expected success with correct version, got %v", err)
	}
}

func TestCluster_EnsureNetworkCIDR_FirstWriterWins(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	ctx := context.Background()
	defaultCIDR := mustPrefix("10.0.0.0/16")

	// First write sets the CIDR.
	cidr, err := regA.EnsureNetworkCIDR(ctx, mustPrefix("10.42.0.0/16"), "", defaultCIDR)
	if err != nil {
		t.Fatal(err)
	}
	if cidr.String() != "10.42.0.0/16" {
		t.Errorf("expected 10.42.0.0/16, got %s", cidr)
	}

	// Second write should get the first one's CIDR, not its own.
	cidr, err = regB.EnsureNetworkCIDR(ctx, mustPrefix("10.99.0.0/16"), "", defaultCIDR)
	if err != nil {
		t.Fatal(err)
	}
	if cidr.String() != "10.42.0.0/16" {
		t.Errorf("expected first-writer-wins 10.42.0.0/16, got %s", cidr)
	}
}

func TestCluster_Heartbeat(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	_, hbCh, err := regB.SubscribeHeartbeats(ctx)
	if err != nil {
		t.Fatal(err)
	}

	if err := regA.BumpHeartbeat(ctx, "node-a", "2025-01-01T00:00:00Z"); err != nil {
		t.Fatal(err)
	}

	select {
	case change := <-hbCh:
		if change.Heartbeat.NodeID != "node-a" {
			t.Errorf("expected heartbeat for node-a, got %q", change.Heartbeat.NodeID)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for heartbeat change")
	}
}

func TestCluster_DeleteMachine(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	reg := c.Registry("node-a")
	_ = c.Registry("node-b") // ensure node-b exists

	ctx := context.Background()
	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1"}
	_ = reg.UpsertMachine(ctx, row, 0)

	if err := reg.DeleteMachine(ctx, "m1"); err != nil {
		t.Fatal(err)
	}

	// Both nodes should see deletion.
	snap := c.Snapshot("node-a")
	if _, ok := snap.Machine("m1"); ok {
		t.Error("expected m1 deleted from node-a")
	}
	snap = c.Snapshot("node-b")
	if _, ok := snap.Machine("m1"); ok {
		t.Error("expected m1 deleted from node-b")
	}
}

func TestCluster_Snapshot(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	reg := c.Registry("node-a")
	ctx := context.Background()
	_ = reg.UpsertMachine(ctx, mesh.MachineRow{ID: "m1"}, 0)
	_ = reg.UpsertMachine(ctx, mesh.MachineRow{ID: "m2"}, 0)

	snap := c.Snapshot("node-a")
	if len(snap.Machines) != 2 {
		t.Errorf("expected 2 machines, got %d", len(snap.Machines))
	}
	if _, ok := snap.Machine("m1"); !ok {
		t.Error("expected m1 in snapshot")
	}
	if _, ok := snap.Machine("nonexistent"); ok {
		t.Error("expected nonexistent to not be in snapshot")
	}
}

func TestCluster_PingRTT(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	c.Registry("node-a")
	c.Registry("node-b")

	c.SetLink("node-a", "node-b", LinkConfig{PingRTT: 50 * time.Millisecond})

	rtt := c.PingRTT("node-a", "node-b")
	if rtt != 50*time.Millisecond {
		t.Errorf("expected 50ms RTT, got %v", rtt)
	}

	// Blocked link returns -1.
	c.BlockLink("node-a", "node-b")
	rtt = c.PingRTT("node-a", "node-b")
	if rtt != -1 {
		t.Errorf("expected -1 for blocked link, got %v", rtt)
	}
}

func TestCluster_DialFunc(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	c.Registry("node-a")
	c.Registry("node-b")

	// Register node-b's address so DialFunc can resolve it.
	c.RegisterAddr("node-b", "5.6.7.8:51820")
	c.SetLink("node-a", "node-b", LinkConfig{PingRTT: 25 * time.Millisecond})

	dial := c.DialFunc("node-a")
	ctx := context.Background()

	rtt, err := dial(ctx, "5.6.7.8:51820")
	if err != nil {
		t.Fatal(err)
	}
	if rtt != 25*time.Millisecond {
		t.Errorf("expected 25ms RTT, got %v", rtt)
	}

	// Blocked link should return error.
	c.BlockLink("node-a", "node-b")
	_, err = dial(ctx, "5.6.7.8:51820")
	if err == nil {
		t.Error("expected error for blocked link")
	}
}

func TestCluster_KillNode_RegistryReturnsError(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")

	ctx := context.Background()

	c.KillNode("node-a")

	// All ops on killed node should return ErrNodeDead.
	if err := regA.EnsureMachineTable(ctx); !errors.Is(err, ErrNodeDead) {
		t.Errorf("EnsureMachineTable: expected ErrNodeDead, got %v", err)
	}
	if _, err := regA.ListMachineRows(ctx); !errors.Is(err, ErrNodeDead) {
		t.Errorf("ListMachineRows: expected ErrNodeDead, got %v", err)
	}
	if err := regA.UpsertMachine(ctx, mesh.MachineRow{ID: "m1"}, 0); !errors.Is(err, ErrNodeDead) {
		t.Errorf("UpsertMachine: expected ErrNodeDead, got %v", err)
	}
	if err := regA.DeleteMachine(ctx, "m1"); !errors.Is(err, ErrNodeDead) {
		t.Errorf("DeleteMachine: expected ErrNodeDead, got %v", err)
	}
	if _, _, err := regA.SubscribeMachines(ctx); !errors.Is(err, ErrNodeDead) {
		t.Errorf("SubscribeMachines: expected ErrNodeDead, got %v", err)
	}
	if err := regA.BumpHeartbeat(ctx, "node-a", "2025-01-01T00:00:00Z"); !errors.Is(err, ErrNodeDead) {
		t.Errorf("BumpHeartbeat: expected ErrNodeDead, got %v", err)
	}

	// Other node should be unaffected.
	if err := regB.UpsertMachine(ctx, mesh.MachineRow{ID: "m2", PublicKey: "pk2"}, 0); err != nil {
		t.Errorf("node-b UpsertMachine should work, got %v", err)
	}
	rows, err := regB.ListMachineRows(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || rows[0].ID != "m2" {
		t.Errorf("expected node-b to have m2, got %v", rows)
	}
}

func TestCluster_KillNode_ReplicationBlocked(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")
	regC := c.Registry("node-c")

	ctx := context.Background()

	// Kill B, write on A — C sees it, B doesn't.
	c.KillNode("node-b")

	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1", Endpoint: "1.2.3.4:51820"}
	if err := regA.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}

	// C should see the write (instant replication).
	rows, err := regC.ListMachineRows(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 || rows[0].ID != "m1" {
		t.Errorf("expected node-c to see m1, got %v", rows)
	}

	// B's local state should be empty (never received replication).
	snap := c.Snapshot("node-b")
	if len(snap.Machines) != 0 {
		t.Errorf("expected killed node-b to have 0 machines, got %d", len(snap.Machines))
	}

	// Also verify B's registry is dead.
	_, err = regB.ListMachineRows(ctx)
	if !errors.Is(err, ErrNodeDead) {
		t.Errorf("expected ErrNodeDead from killed node-b, got %v", err)
	}
}

func TestCluster_RestartNode_SyncCatchesUp(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")
	c.Registry("node-c") // ensure node-c exists

	ctx := context.Background()

	// Write initial data visible to all.
	row := mesh.MachineRow{ID: "m-existing", PublicKey: "pk0", Version: 0}
	if err := regA.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}

	// Kill B, write new data on A.
	c.KillNode("node-b")

	newRow := mesh.MachineRow{ID: "m-new", PublicKey: "pk1", Endpoint: "1.2.3.4:51820"}
	if err := regA.UpsertMachine(ctx, newRow, 0); err != nil {
		t.Fatal(err)
	}

	// B should NOT have the new data.
	snap := c.Snapshot("node-b")
	if _, ok := snap.Machine("m-new"); ok {
		t.Error("killed node-b should not have m-new")
	}

	// Restart B — anti-entropy should sync the new data.
	c.RestartNode("node-b")

	snap = c.Snapshot("node-b")
	if _, ok := snap.Machine("m-new"); !ok {
		t.Error("restarted node-b should have m-new after anti-entropy")
	}

	// Registry should work again.
	rows, err := regB.ListMachineRows(ctx)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, r := range rows {
		if r.ID == "m-new" {
			found = true
		}
	}
	if !found {
		t.Error("restarted node-b ListMachineRows should include m-new")
	}
}

func TestCluster_KillDeleteRestart_DeletionPropagated(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	c.Registry("node-b")

	ctx := context.Background()

	// Write a machine visible to all.
	row := mesh.MachineRow{ID: "m-doomed", PublicKey: "pk1"}
	if err := regA.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}

	// Verify B has it.
	snap := c.Snapshot("node-b")
	if _, ok := snap.Machine("m-doomed"); !ok {
		t.Fatal("node-b should have m-doomed before kill")
	}

	// Kill B, delete the machine on A.
	c.KillNode("node-b")
	if err := regA.DeleteMachine(ctx, "m-doomed"); err != nil {
		t.Fatal(err)
	}

	// B still has it locally (state preserved).
	snap = c.Snapshot("node-b")
	if _, ok := snap.Machine("m-doomed"); !ok {
		t.Error("killed node-b should still have m-doomed locally")
	}

	// Restart B — anti-entropy should remove the machine since no peer has it.
	c.RestartNode("node-b")

	snap = c.Snapshot("node-b")
	if _, ok := snap.Machine("m-doomed"); ok {
		t.Error("restarted node-b should not have m-doomed (deleted on all peers)")
	}
}

func TestCluster_KillRestart_LocalStatePreserved(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	c.Registry("node-b")

	ctx := context.Background()

	// Write data on A.
	row := mesh.MachineRow{ID: "m1", PublicKey: "pk1"}
	if err := regA.UpsertMachine(ctx, row, 0); err != nil {
		t.Fatal(err)
	}
	if err := regA.BumpHeartbeat(ctx, "node-a", "2025-01-01T00:00:00Z"); err != nil {
		t.Fatal(err)
	}

	// Kill A.
	c.KillNode("node-a")

	// Local state should still be there (via Snapshot, which bypasses killed check).
	snap := c.Snapshot("node-a")
	if _, ok := snap.Machine("m1"); !ok {
		t.Error("killed node-a should preserve m1 in local state")
	}
	if len(snap.Heartbeats) == 0 {
		t.Error("killed node-a should preserve heartbeats in local state")
	}

	// Restart A.
	c.RestartNode("node-a")

	// Data should still be there and registry should work.
	rows, err := regA.ListMachineRows(ctx)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, r := range rows {
		if r.ID == "m1" {
			found = true
		}
	}
	if !found {
		t.Error("restarted node-a should still have m1")
	}
}

func TestCluster_ThreeNode_SplitBrain_Convergence(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	c := NewCluster(clock)

	regA := c.Registry("node-a")
	regB := c.Registry("node-b")
	c.Registry("node-c")

	ctx := context.Background()

	// Shared machine visible to all.
	shared := mesh.MachineRow{ID: "m-shared", PublicKey: "pk-shared"}
	if err := regA.UpsertMachine(ctx, shared, 0); err != nil {
		t.Fatal(err)
	}

	// Kill C.
	c.KillNode("node-c")

	// Independent writes on A and B while C is dead.
	mA := mesh.MachineRow{ID: "m-a", PublicKey: "pk-a", Endpoint: "1.1.1.1:51820"}
	if err := regA.UpsertMachine(ctx, mA, 0); err != nil {
		t.Fatal(err)
	}
	mB := mesh.MachineRow{ID: "m-b", PublicKey: "pk-b", Endpoint: "2.2.2.2:51820"}
	if err := regB.UpsertMachine(ctx, mB, 0); err != nil {
		t.Fatal(err)
	}

	// Delete shared machine on A (replicates to B since they're alive).
	if err := regA.DeleteMachine(ctx, "m-shared"); err != nil {
		t.Fatal(err)
	}

	// C still has old state.
	snap := c.Snapshot("node-c")
	if _, ok := snap.Machine("m-shared"); !ok {
		t.Error("killed node-c should still have m-shared")
	}
	if _, ok := snap.Machine("m-a"); ok {
		t.Error("killed node-c should not have m-a")
	}

	// Restart C — anti-entropy merges from A and B.
	c.RestartNode("node-c")

	snap = c.Snapshot("node-c")

	// C should have m-a and m-b.
	if _, ok := snap.Machine("m-a"); !ok {
		t.Error("restarted node-c should have m-a")
	}
	if _, ok := snap.Machine("m-b"); !ok {
		t.Error("restarted node-c should have m-b")
	}

	// m-shared should be gone (deleted on all peers).
	if _, ok := snap.Machine("m-shared"); ok {
		t.Error("restarted node-c should not have m-shared (deleted on peers)")
	}
}
