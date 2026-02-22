package fake

import (
	"context"
	"errors"
	"testing"
	"testing/synctest"
	"time"

	"ployz/internal/mesh"
)

func TestHarness_BasicConvergence(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := NewHarness(HarnessConfig{
			NodeIDs: []string{"a", "b", "c"},
		})
		h.Start(context.Background())

		// Let workers subscribe and reconcile initial snapshot.
		synctest.Wait()

		for _, id := range []string{"a", "b", "c"} {
			node := h.Node(id)
			node.Reconciler.mu.Lock()
			rows := node.Reconciler.LastRows
			node.Reconciler.mu.Unlock()

			if len(rows) != 3 {
				t.Errorf("node %s: expected 3 machines, got %d", id, len(rows))
			}

			// Verify all three node IDs are present.
			seen := make(map[string]bool)
			for _, r := range rows {
				seen[r.ID] = true
			}
			for _, want := range []string{"a", "b", "c"} {
				if !seen[want] {
					t.Errorf("node %s: missing machine %s in reconciled rows", id, want)
				}
			}
		}

		h.Stop()
	})
}

func TestHarness_PartitionAndHeal(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := NewHarness(HarnessConfig{
			NodeIDs: []string{"a", "b", "c"},
		})
		h.Start(context.Background())
		synctest.Wait()

		// Partition: A isolated from {B, C}.
		h.Cluster.Partition([]string{"a"}, []string{"b", "c"})

		// Upsert a new machine on A's registry.
		ctx := context.Background()
		newRow := mesh.MachineRow{
			ID:        "m-new",
			PublicKey: "pk-new",
			Endpoint:  "9.9.9.9:51820",
			Subnet:    "10.210.99.0/24",
		}
		_ = h.Node("a").Registry.UpsertMachine(ctx, newRow, 0)

		// Drain delivers pending writes (but partition blocks A→B, A→C).
		h.Cluster.Drain()
		synctest.Wait()

		// B and C should NOT see the new machine.
		for _, id := range []string{"b", "c"} {
			snap := h.Cluster.Snapshot(id)
			if _, found := snap.Machine("m-new"); found {
				t.Errorf("node %s should NOT see m-new during partition", id)
			}
		}

		// A should see it locally.
		snapA := h.Cluster.Snapshot("a")
		if _, found := snapA.Machine("m-new"); !found {
			t.Error("node a should see m-new locally")
		}

		// Heal and drain.
		h.Cluster.Heal()
		// Re-upsert to trigger fan-out now that links are open.
		_ = h.Node("a").Registry.UpsertMachine(ctx, newRow, 1)
		h.Cluster.Drain()
		synctest.Wait()

		// Now all nodes should see it.
		for _, id := range []string{"a", "b", "c"} {
			snap := h.Cluster.Snapshot(id)
			if _, found := snap.Machine("m-new"); !found {
				t.Errorf("node %s should see m-new after heal", id)
			}
		}

		h.Stop()
	})
}

func TestHarness_FailureInjection(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := NewHarness(HarnessConfig{
			NodeIDs: []string{"a", "b", "c"},
		})

		// Inject SubscribeMachines error on node B before starting.
		injectedErr := errors.New("injected subscribe failure")
		h.Node("b").Registry.SubscribeMachinesErr = func(ctx context.Context) error {
			return injectedErr
		}

		ctx, cancel := context.WithCancel(context.Background())
		h.Start(ctx)

		// B will fail its subscribe retry loop and block on time.After(1s).
		// A and C should converge normally.
		synctest.Wait()

		// A and C should have reconciled.
		for _, id := range []string{"a", "c"} {
			node := h.Node(id)
			node.Reconciler.mu.Lock()
			rows := node.Reconciler.LastRows
			node.Reconciler.mu.Unlock()

			if len(rows) != 3 {
				t.Errorf("node %s: expected 3 machines, got %d", id, len(rows))
			}
		}

		// B should NOT have reconciled (subscribe failed, no snapshot delivered).
		nodeB := h.Node("b")
		nodeB.Reconciler.mu.Lock()
		bRows := nodeB.Reconciler.LastRows
		nodeB.Reconciler.mu.Unlock()
		if len(bRows) != 0 {
			t.Errorf("node b: expected 0 reconciled rows (subscribe failed), got %d", len(bRows))
		}

		cancel()
		h.Stop()

		// B's worker should have exited with context cancelled (it was stuck in retry loop).
		bErr := nodeB.Err()
		if bErr == nil || !errors.Is(bErr, context.Canceled) {
			t.Errorf("node b: expected context.Canceled, got %v", bErr)
		}
	})
}

func TestHarness_Latency(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := NewHarness(HarnessConfig{
			NodeIDs: []string{"a", "b", "c"},
		})
		h.Start(context.Background())
		synctest.Wait()

		// Set 200ms replication latency from A to B.
		h.Cluster.SetLink("a", "b", LinkConfig{Latency: 200 * time.Millisecond})

		// Upsert a new machine on A.
		ctx := context.Background()
		newRow := mesh.MachineRow{
			ID:        "m-delayed",
			PublicKey: "pk-delayed",
			Endpoint:  "8.8.8.8:51820",
			Subnet:    "10.210.88.0/24",
		}
		_ = h.Node("a").Registry.UpsertMachine(ctx, newRow, 0)

		// Tick delivers only writes whose deliverAt <= now.
		// Since latency is 200ms and no time has advanced, B shouldn't have it.
		h.Cluster.Tick()
		synctest.Wait()

		snapB := h.Cluster.Snapshot("b")
		if _, found := snapB.Machine("m-delayed"); found {
			t.Error("node b should NOT see m-delayed before latency expires")
		}

		// C has no latency configured, should see it immediately.
		snapC := h.Cluster.Snapshot("c")
		if _, found := snapC.Machine("m-delayed"); !found {
			t.Error("node c should see m-delayed immediately (no latency)")
		}

		// Advance fake time past the 200ms latency.
		time.Sleep(200 * time.Millisecond)

		// Tick delivers the pending write now that fake time has advanced.
		h.Cluster.Tick()
		synctest.Wait()

		snapB = h.Cluster.Snapshot("b")
		if _, found := snapB.Machine("m-delayed"); !found {
			t.Error("node b should see m-delayed after latency expires")
		}

		// Verify B's worker reconciled the new machine.
		nodeB := h.Node("b")
		nodeB.Reconciler.mu.Lock()
		bRows := nodeB.Reconciler.LastRows
		nodeB.Reconciler.mu.Unlock()

		found := false
		for _, r := range bRows {
			if r.ID == "m-delayed" {
				found = true
				break
			}
		}
		if !found {
			ids := make([]string, len(bRows))
			for i, r := range bRows {
				ids[i] = r.ID
			}
			t.Errorf("node b reconciler should include m-delayed, got %v", ids)
		}

		h.Stop()
	})
}

func TestHarness_NodeAccessor(t *testing.T) {
	h := NewHarness(HarnessConfig{
		NodeIDs: []string{"x", "y"},
	})
	if h.Node("x") == nil {
		t.Error("expected non-nil node for 'x'")
	}
	if h.Node("z") != nil {
		t.Error("expected nil node for unknown 'z'")
	}
	ids := h.Nodes()
	if len(ids) != 2 {
		t.Errorf("expected 2 node IDs, got %d", len(ids))
	}
}
