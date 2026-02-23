package scenario

import (
	"context"
	"path/filepath"
	"testing"
	"time"

	"ployz/internal/adapter/fake/cluster"
	"ployz/internal/mesh"
)

func TestNewRequiresNodeIDs(t *testing.T) {
	_, err := New(context.Background(), Config{})
	if err == nil {
		t.Fatal("expected error when NodeIDs is empty")
	}
}

func TestNewDuplicateNodeID(t *testing.T) {
	_, err := New(context.Background(), Config{NodeIDs: []string{"a", "a"}})
	if err == nil {
		t.Fatal("expected error for duplicate node IDs")
	}
}

func TestScenarioNodeAccessors(t *testing.T) {
	ctx := t.Context()

	s := MustNew(t, ctx, Config{
		NodeIDs:      []string{"a", "b"},
		DataRootBase: "/tmp/scenario-accessors",
	})

	if s.Node("a") == nil {
		t.Fatal("expected node a")
	}
	if s.Node("missing") != nil {
		t.Fatal("expected nil for missing node")
	}

	ids := s.Nodes()
	if len(ids) != 2 || ids[0] != "a" || ids[1] != "b" {
		t.Fatalf("unexpected node IDs: %v", ids)
	}

	wantRoot := filepath.Join("/tmp/scenario-accessors", "a")
	if s.Node("a").DataRoot != wantRoot {
		t.Fatalf("unexpected data root: got %q want %q", s.Node("a").DataRoot, wantRoot)
	}
}

func TestScenarioDrainDeliversPendingWrites(t *testing.T) {
	ctx := t.Context()

	s := MustNew(t, ctx, Config{NodeIDs: []string{"a", "b"}})

	s.Cluster.SetLink("a", "b", cluster.LinkConfig{Latency: time.Second})
	if err := s.Cluster.Registry("a").UpsertMachine(ctx, mesh.MachineRow{ID: "m1", PublicKey: "pk1"}, 0); err != nil {
		t.Fatalf("upsert machine: %v", err)
	}

	rows, err := s.Cluster.Registry("b").ListMachineRows(ctx)
	if err != nil {
		t.Fatalf("list before drain: %v", err)
	}
	if len(rows) != 0 {
		t.Fatalf("expected no rows before drain, got %d", len(rows))
	}

	s.Drain()

	rows, err = s.Cluster.Registry("b").ListMachineRows(ctx)
	if err != nil {
		t.Fatalf("list after drain: %v", err)
	}
	if len(rows) != 1 || rows[0].ID != "m1" {
		t.Fatalf("unexpected rows after drain: %+v", rows)
	}
}

func TestScenarioAddRemoveNode(t *testing.T) {
	ctx := t.Context()

	s := MustNew(t, ctx, Config{NodeIDs: []string{"a"}})

	if _, err := s.AddNode("b"); err != nil {
		t.Fatalf("AddNode(b): %v", err)
	}

	ids := s.Nodes()
	if len(ids) != 2 || ids[0] != "a" || ids[1] != "b" {
		t.Fatalf("unexpected nodes after AddNode: %v", ids)
	}

	if err := s.RemoveNode("b"); err != nil {
		t.Fatalf("RemoveNode(b): %v", err)
	}
	if s.Node("b") != nil {
		t.Fatal("expected node b to be removed")
	}

	if err := s.RemoveNode("b"); err == nil {
		t.Fatal("expected RemoveNode on missing node to fail")
	}
}

func TestScenarioPartitionWrappers(t *testing.T) {
	ctx := t.Context()

	s := MustNew(t, ctx, Config{NodeIDs: []string{"a", "b"}})

	s.Partition([]string{"a"}, []string{"b"})
	if err := s.Cluster.Registry("a").UpsertMachine(ctx, mesh.MachineRow{ID: "m1", PublicKey: "pk1"}, 0); err != nil {
		t.Fatalf("upsert machine: %v", err)
	}
	s.Drain()

	if _, found := s.Snapshot("b").Machine("m1"); found {
		t.Fatal("node b should not receive m1 while partitioned")
	}

	s.Heal()
	if err := s.Cluster.Registry("a").UpsertMachine(ctx, mesh.MachineRow{ID: "m1", PublicKey: "pk1"}, 1); err != nil {
		t.Fatalf("re-upsert machine after heal: %v", err)
	}
	s.Drain()

	if _, found := s.Snapshot("b").Machine("m1"); !found {
		t.Fatal("node b should receive m1 after heal")
	}
}
