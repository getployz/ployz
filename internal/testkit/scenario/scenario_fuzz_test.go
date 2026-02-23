package scenario

import (
	"context"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"testing"
	"time"

	"ployz/internal/adapter/fake/cluster"
	"ployz/internal/mesh"
)

const maxFuzzNodes = 6

func FuzzNew_NodeValidation(f *testing.F) {
	f.Add("a,b", false)
	f.Add("a,a", false)
	f.Add("", true)
	f.Add(" ,b", false)

	f.Fuzz(func(t *testing.T, raw string, noNodes bool) {
		nodeIDs := parseFuzzNodeIDs(raw, noNodes)
		wantSuccess := shouldBuildScenario(nodeIDs)

		ctx, cancel := context.WithCancel(t.Context())
		defer cancel()

		base := "/tmp/ployz-scenario-fuzz-new"
		s, err := New(ctx, Config{NodeIDs: nodeIDs, DataRootBase: base})
		if !wantSuccess {
			if err == nil {
				t.Fatalf("expected New to fail for node IDs %q", nodeIDs)
			}
			return
		}

		if err != nil {
			t.Fatalf("New error: %v", err)
		}
		if s == nil {
			t.Fatal("expected non-nil scenario")
		}

		got := s.Nodes()
		if len(got) != len(nodeIDs) {
			t.Fatalf("nodes length: got %d want %d", len(got), len(nodeIDs))
		}
		if !sort.StringsAreSorted(got) {
			t.Fatalf("nodes should be sorted: %v", got)
		}

		for _, id := range nodeIDs {
			n := s.Node(id)
			if n == nil {
				t.Fatalf("missing node %q", id)
			}
			wantRoot := filepath.Join(base, id)
			if n.DataRoot != wantRoot {
				t.Fatalf("node %q data root: got %q want %q", id, n.DataRoot, wantRoot)
			}
		}
	})
}

func FuzzNodes_DeterministicSorted(f *testing.F) {
	f.Add("beta,alpha")
	f.Add("node-1,node-1,node-2")
	f.Add("   ")

	f.Fuzz(func(t *testing.T, raw string) {
		nodeIDs := validNodeIDsFromRaw(raw)

		ctx, cancel := context.WithCancel(t.Context())
		defer cancel()

		s, err := New(ctx, Config{NodeIDs: nodeIDs, DataRootBase: "/tmp/ployz-scenario-fuzz-nodes"})
		if err != nil {
			t.Fatalf("New error: %v", err)
		}

		first := s.Nodes()
		second := s.Nodes()
		if !slices.Equal(first, second) {
			t.Fatalf("Nodes() should be deterministic: first=%v second=%v", first, second)
		}
		if !sort.StringsAreSorted(first) {
			t.Fatalf("Nodes() should be sorted: %v", first)
		}

		want := append([]string(nil), nodeIDs...)
		sort.Strings(want)
		if !slices.Equal(first, want) {
			t.Fatalf("Nodes() mismatch: got=%v want=%v", first, want)
		}
	})
}

func FuzzDrain_DeliversReplicatedWrites(f *testing.F) {
	f.Add("a", "b", "m1", "pk1", uint8(0))
	f.Add("left", "right", "machine", "public", uint8(15))

	f.Fuzz(func(t *testing.T, rawA string, rawB string, machineID string, publicKey string, latencyMs uint8) {
		nodeA := normalizeID(rawA, "a")
		nodeB := normalizeID(rawB, "b")
		if nodeA == nodeB {
			nodeB = nodeB + "-peer"
		}

		machineID = normalizeID(machineID, "m1")
		publicKey = normalizeID(publicKey, "pk1")

		ctx, cancel := context.WithCancel(t.Context())
		defer cancel()

		s, err := New(ctx, Config{
			NodeIDs:      []string{nodeA, nodeB},
			DataRootBase: "/tmp/ployz-scenario-fuzz-drain",
		})
		if err != nil {
			t.Fatalf("New error: %v", err)
		}

		latency := time.Duration(latencyMs%20) * time.Millisecond
		s.Cluster.SetLink(nodeA, nodeB, cluster.LinkConfig{Latency: latency})

		if err := s.Cluster.Registry(nodeA).UpsertMachine(ctx, mesh.MachineRow{ID: machineID, PublicKey: publicKey}, 0); err != nil {
			t.Fatalf("upsert machine: %v", err)
		}

		s.Drain()

		rows, err := s.Cluster.Registry(nodeB).ListMachineRows(ctx)
		if err != nil {
			t.Fatalf("list machine rows: %v", err)
		}

		found := false
		for _, row := range rows {
			if row.ID == machineID && row.PublicKey == publicKey {
				found = true
				break
			}
		}
		if !found {
			t.Fatalf("replicated machine not found: node=%q machine=%q rows=%+v", nodeB, machineID, rows)
		}
	})
}

func parseFuzzNodeIDs(raw string, noNodes bool) []string {
	if noNodes {
		return nil
	}

	parts := strings.Split(raw, ",")
	if len(parts) > maxFuzzNodes {
		parts = parts[:maxFuzzNodes]
	}
	return parts
}

func shouldBuildScenario(nodeIDs []string) bool {
	if len(nodeIDs) == 0 {
		return false
	}

	seen := make(map[string]struct{}, len(nodeIDs))
	for _, id := range nodeIDs {
		if strings.TrimSpace(id) == "" {
			return false
		}
		if _, exists := seen[id]; exists {
			return false
		}
		seen[id] = struct{}{}
	}

	return true
}

func validNodeIDsFromRaw(raw string) []string {
	parts := strings.FieldsFunc(raw, func(r rune) bool {
		switch r {
		case ',', ';', '|', ':', '/', '\\', '\t', '\n', ' ':
			return true
		default:
			return false
		}
	})

	ids := make([]string, 0, maxFuzzNodes)
	seen := make(map[string]struct{}, maxFuzzNodes)
	for _, p := range parts {
		id := strings.TrimSpace(p)
		if id == "" {
			continue
		}
		if _, exists := seen[id]; exists {
			continue
		}
		ids = append(ids, id)
		seen[id] = struct{}{}
		if len(ids) == maxFuzzNodes {
			break
		}
	}

	if len(ids) == 0 {
		return []string{"node-0"}
	}

	return ids
}

func normalizeID(raw string, fallback string) string {
	id := strings.TrimSpace(raw)
	if id == "" {
		return fallback
	}
	return id
}
