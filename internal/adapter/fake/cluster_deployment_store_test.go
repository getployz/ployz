package fake

import (
	"errors"
	"testing"
	"time"

	"ployz/internal/deploy"
)

func TestClusterDeploymentStore_OwnershipReplicates(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)

	storeA := NewClusterDeploymentStore(cluster, "node-a")
	storeB := NewClusterDeploymentStore(cluster, "node-b")

	ctx := t.Context()
	row := deploy.DeploymentRow{
		ID:        "d1",
		Namespace: "ns",
		Status:    "in_progress",
		CreatedAt: "2025-01-01T00:00:00Z",
		UpdatedAt: "2025-01-01T00:00:00Z",
	}

	if err := storeA.InsertDeployment(ctx, row); err != nil {
		t.Fatalf("InsertDeployment() error = %v", err)
	}

	if err := storeA.AcquireOwnership(ctx, "d1", "node-a", "2025-01-01T00:00:01Z"); err != nil {
		t.Fatalf("AcquireOwnership() error = %v", err)
	}

	if err := storeB.CheckOwnership(ctx, "d1", "node-a"); err != nil {
		t.Fatalf("CheckOwnership() error = %v", err)
	}
}

func TestClusterDeploymentStore_RespectsLatencyUntilDrain(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)
	cluster.SetLink("node-a", "node-b", LinkConfig{Latency: 200 * time.Millisecond})

	storeA := NewClusterDeploymentStore(cluster, "node-a")
	storeB := NewClusterDeploymentStore(cluster, "node-b")

	ctx := t.Context()
	row := deploy.DeploymentRow{ID: "d1", Namespace: "ns", Status: "in_progress", CreatedAt: "2025-01-01T00:00:00Z", UpdatedAt: "2025-01-01T00:00:00Z"}

	if err := storeA.InsertDeployment(ctx, row); err != nil {
		t.Fatalf("InsertDeployment() error = %v", err)
	}

	if _, ok, err := storeB.GetDeployment(ctx, "d1"); err != nil {
		t.Fatalf("GetDeployment() error = %v", err)
	} else if ok {
		t.Fatal("before Drain: deployment should not be visible on node-b")
	}

	cluster.Drain()

	got, ok, err := storeB.GetDeployment(ctx, "d1")
	if err != nil {
		t.Fatalf("GetDeployment() error = %v", err)
	}
	if !ok || got.ID != "d1" {
		t.Fatalf("after Drain: got %+v (ok=%v), want d1", got, ok)
	}
}

func TestClusterDeploymentStore_FaultFailOnce(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)

	storeA := NewClusterDeploymentStore(cluster, "node-a")
	storeB := NewClusterDeploymentStore(cluster, "node-b")
	ctx := t.Context()
	injected := errors.New("injected")
	storeA.FailOnce(FaultClusterDeploymentStoreInsert, injected)

	row := deploy.DeploymentRow{ID: "d1", Namespace: "ns", Status: "in_progress", CreatedAt: "2025-01-01T00:00:00Z", UpdatedAt: "2025-01-01T00:00:00Z"}
	err := storeA.InsertDeployment(ctx, row)
	if !errors.Is(err, injected) {
		t.Fatalf("first InsertDeployment() error = %v, want injected", err)
	}

	_, ok, err := storeB.GetDeployment(ctx, "d1")
	if err != nil {
		t.Fatalf("GetDeployment(node-b) error = %v", err)
	}
	if ok {
		t.Fatal("before successful retry: deployment should not be visible on node-b")
	}

	err = storeA.InsertDeployment(ctx, row)
	if err != nil {
		t.Fatalf("second InsertDeployment() error = %v, want nil", err)
	}

	got, ok, err := storeB.GetDeployment(ctx, "d1")
	if err != nil {
		t.Fatalf("GetDeployment(node-b) error = %v", err)
	}
	if !ok || got.ID != "d1" {
		t.Fatalf("after successful retry: got %+v (ok=%v), want d1", got, ok)
	}
}

func TestClusterDeploymentStore_FaultHook(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)

	storeA := NewClusterDeploymentStore(cluster, "node-a")
	storeB := NewClusterDeploymentStore(cluster, "node-b")
	ctx := t.Context()
	injected := errors.New("hook injected")
	var seenID string

	storeA.SetFaultHook(FaultClusterDeploymentStoreInsert, func(args ...any) error {
		if len(args) != 2 {
			t.Fatalf("hook args len = %d, want 2", len(args))
		}
		row, ok := args[1].(deploy.DeploymentRow)
		if !ok {
			t.Fatalf("hook arg[1] type = %T, want deploy.DeploymentRow", args[1])
		}
		seenID = row.ID
		if row.ID == "d1" {
			return injected
		}
		return nil
	})

	err := storeA.InsertDeployment(ctx, deploy.DeploymentRow{ID: "d1", Namespace: "ns", Status: "in_progress", CreatedAt: "2025-01-01T00:00:00Z", UpdatedAt: "2025-01-01T00:00:00Z"})
	if !errors.Is(err, injected) {
		t.Fatalf("InsertDeployment(d1) error = %v, want injected", err)
	}
	if seenID != "d1" {
		t.Fatalf("hook seen ID = %q, want d1", seenID)
	}

	err = storeA.InsertDeployment(ctx, deploy.DeploymentRow{ID: "d2", Namespace: "ns", Status: "in_progress", CreatedAt: "2025-01-01T00:00:00Z", UpdatedAt: "2025-01-01T00:00:00Z"})
	if err != nil {
		t.Fatalf("InsertDeployment(d2) error = %v, want nil", err)
	}

	_, ok, err := storeB.GetDeployment(ctx, "d1")
	if err != nil {
		t.Fatalf("GetDeployment(node-b,d1) error = %v", err)
	}
	if ok {
		t.Fatal("node-b should not see d1")
	}

	got, ok, err := storeB.GetDeployment(ctx, "d2")
	if err != nil {
		t.Fatalf("GetDeployment(node-b,d2) error = %v", err)
	}
	if !ok || got.ID != "d2" {
		t.Fatalf("GetDeployment(node-b,d2) = %+v (ok=%v), want d2", got, ok)
	}
}

func TestClusterDeploymentStore_FaultPoints(t *testing.T) {
	baseRow := deploy.DeploymentRow{
		ID:        "d1",
		Namespace: "ns",
		Status:    "in_progress",
		CreatedAt: "2025-01-01T00:00:00Z",
		UpdatedAt: "2025-01-01T00:00:00Z",
	}

	setupBase := func(t *testing.T, store *ClusterDeploymentStore) {
		t.Helper()
		if err := store.InsertDeployment(t.Context(), baseRow); err != nil {
			t.Fatalf("setup InsertDeployment() error = %v", err)
		}
	}

	setupOwned := func(t *testing.T, store *ClusterDeploymentStore) {
		t.Helper()
		setupBase(t, store)
		if err := store.AcquireOwnership(t.Context(), "d1", "node-a", "2025-01-01T00:00:01Z"); err != nil {
			t.Fatalf("setup AcquireOwnership() error = %v", err)
		}
	}

	tests := []struct {
		name  string
		point string
		setup func(*testing.T, *ClusterDeploymentStore)
		run   func(*testing.T, *ClusterDeploymentStore) error
	}{
		{
			name:  "ensure table",
			point: FaultClusterDeploymentStoreEnsureTable,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				return store.EnsureDeploymentTable(t.Context())
			},
		},
		{
			name:  "insert",
			point: FaultClusterDeploymentStoreInsert,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				return store.InsertDeployment(t.Context(), baseRow)
			},
		},
		{
			name:  "update",
			point: FaultClusterDeploymentStoreUpdate,
			setup: setupBase,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				updated := baseRow
				updated.Status = "succeeded"
				updated.UpdatedAt = "2025-01-01T00:01:00Z"
				return store.UpdateDeployment(t.Context(), updated)
			},
		},
		{
			name:  "get",
			point: FaultClusterDeploymentStoreGet,
			setup: setupBase,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				row, ok, err := store.GetDeployment(t.Context(), "d1")
				if err != nil {
					return err
				}
				if !ok || row.ID != "d1" {
					return errors.New("GetDeployment() did not return d1")
				}
				return nil
			},
		},
		{
			name:  "get active",
			point: FaultClusterDeploymentStoreGetActive,
			setup: setupBase,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				row, ok, err := store.GetActiveDeployment(t.Context(), "ns")
				if err != nil {
					return err
				}
				if !ok || row.ID != "d1" {
					return errors.New("GetActiveDeployment() did not return d1")
				}
				return nil
			},
		},
		{
			name:  "list by namespace",
			point: FaultClusterDeploymentStoreListByNamespace,
			setup: setupBase,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				rows, err := store.ListByNamespace(t.Context(), "ns")
				if err != nil {
					return err
				}
				if len(rows) == 0 {
					return errors.New("ListByNamespace() returned no rows")
				}
				return nil
			},
		},
		{
			name:  "latest successful",
			point: FaultClusterDeploymentStoreLatestSuccessful,
			setup: func(t *testing.T, store *ClusterDeploymentStore) {
				success := baseRow
				success.Status = "succeeded"
				if err := store.InsertDeployment(t.Context(), success); err != nil {
					t.Fatalf("setup InsertDeployment() error = %v", err)
				}
			},
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				row, ok, err := store.LatestSuccessful(t.Context(), "ns")
				if err != nil {
					return err
				}
				if !ok || row.ID != "d1" {
					return errors.New("LatestSuccessful() did not return d1")
				}
				return nil
			},
		},
		{
			name:  "delete",
			point: FaultClusterDeploymentStoreDelete,
			setup: setupBase,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				return store.DeleteDeployment(t.Context(), "d1")
			},
		},
		{
			name:  "acquire ownership",
			point: FaultClusterDeploymentStoreAcquireOwnership,
			setup: setupBase,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				return store.AcquireOwnership(t.Context(), "d1", "node-a", "2025-01-01T00:00:01Z")
			},
		},
		{
			name:  "check ownership",
			point: FaultClusterDeploymentStoreCheckOwnership,
			setup: setupOwned,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				return store.CheckOwnership(t.Context(), "d1", "node-a")
			},
		},
		{
			name:  "bump heartbeat",
			point: FaultClusterDeploymentStoreBumpHeartbeat,
			setup: setupOwned,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				return store.BumpOwnershipHeartbeat(t.Context(), "d1", "node-a", "2025-01-01T00:00:02Z")
			},
		},
		{
			name:  "release ownership",
			point: FaultClusterDeploymentStoreReleaseOwnership,
			setup: setupOwned,
			run: func(t *testing.T, store *ClusterDeploymentStore) error {
				return store.ReleaseOwnership(t.Context(), "d1")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
			cluster := NewCluster(clock)
			store := NewClusterDeploymentStore(cluster, "node-a")

			if tt.setup != nil {
				tt.setup(t, store)
			}

			injected := errors.New("injected")
			store.FailOnce(tt.point, injected)

			err := tt.run(t, store)
			if !errors.Is(err, injected) {
				t.Fatalf("first call error = %v, want injected", err)
			}

			err = tt.run(t, store)
			if err != nil {
				t.Fatalf("second call error = %v, want nil", err)
			}
		})
	}
}
