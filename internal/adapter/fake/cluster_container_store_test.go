package fake

import (
	"errors"
	"testing"
	"time"

	"ployz/internal/deploy"
)

func TestClusterContainerStore_ReplicatesAcrossNodes(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)

	storeA := NewClusterContainerStore(cluster, "node-a")
	storeB := NewClusterContainerStore(cluster, "node-b")

	ctx := t.Context()
	row := deploy.ContainerRow{
		ID:            "c1",
		Namespace:     "ns",
		DeployID:      "d1",
		Service:       "api",
		MachineID:     "node-a",
		ContainerName: "ns-api-1",
		CreatedAt:     "2025-01-01T00:00:00Z",
		UpdatedAt:     "2025-01-01T00:00:00Z",
	}

	if err := storeA.InsertContainer(ctx, row); err != nil {
		t.Fatalf("InsertContainer() error = %v", err)
	}

	aRows, err := storeA.ListContainersByNamespace(ctx, "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace(node-a) error = %v", err)
	}
	if len(aRows) != 1 || aRows[0].ID != "c1" {
		t.Fatalf("ListContainersByNamespace(node-a) = %+v, want c1", aRows)
	}

	bRows, err := storeB.ListContainersByNamespace(ctx, "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace(node-b) error = %v", err)
	}
	if len(bRows) != 1 || bRows[0].ID != "c1" {
		t.Fatalf("ListContainersByNamespace(node-b) = %+v, want replicated c1", bRows)
	}
}

func TestClusterContainerStore_RespectsLatencyUntilTick(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)
	cluster.SetLink("node-a", "node-b", LinkConfig{Latency: 200 * time.Millisecond})

	storeA := NewClusterContainerStore(cluster, "node-a")
	storeB := NewClusterContainerStore(cluster, "node-b")

	ctx := t.Context()
	row := deploy.ContainerRow{ID: "c1", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "node-a", ContainerName: "ns-api-1"}

	if err := storeA.InsertContainer(ctx, row); err != nil {
		t.Fatalf("InsertContainer() error = %v", err)
	}

	bRows, err := storeB.ListContainersByNamespace(ctx, "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", err)
	}
	if len(bRows) != 0 {
		t.Fatalf("before Tick: got %+v, want empty", bRows)
	}

	clock.Advance(200 * time.Millisecond)
	cluster.Tick()

	bRows, err = storeB.ListContainersByNamespace(ctx, "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", err)
	}
	if len(bRows) != 1 || bRows[0].ID != "c1" {
		t.Fatalf("after Tick: got %+v, want c1", bRows)
	}
}

func TestClusterContainerStore_FaultFailOnce(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)

	storeA := NewClusterContainerStore(cluster, "node-a")
	storeB := NewClusterContainerStore(cluster, "node-b")
	ctx := t.Context()
	injected := errors.New("injected")
	storeA.FailOnce(FaultClusterContainerStoreInsert, injected)

	row := deploy.ContainerRow{ID: "c1", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "node-a", ContainerName: "ns-api-1"}
	err := storeA.InsertContainer(ctx, row)
	if !errors.Is(err, injected) {
		t.Fatalf("first InsertContainer() error = %v, want injected", err)
	}

	bRows, err := storeB.ListContainersByNamespace(ctx, "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace(node-b) error = %v", err)
	}
	if len(bRows) != 0 {
		t.Fatalf("before successful retry: got %+v, want empty", bRows)
	}

	err = storeA.InsertContainer(ctx, row)
	if err != nil {
		t.Fatalf("second InsertContainer() error = %v, want nil", err)
	}

	bRows, err = storeB.ListContainersByNamespace(ctx, "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace(node-b) error = %v", err)
	}
	if len(bRows) != 1 || bRows[0].ID != "c1" {
		t.Fatalf("after successful retry: got %+v, want c1", bRows)
	}
}

func TestClusterContainerStore_FaultHook(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)

	storeA := NewClusterContainerStore(cluster, "node-a")
	storeB := NewClusterContainerStore(cluster, "node-b")
	ctx := t.Context()
	injected := errors.New("hook injected")
	var seenID string

	storeA.SetFaultHook(FaultClusterContainerStoreInsert, func(args ...any) error {
		if len(args) != 2 {
			t.Fatalf("hook args len = %d, want 2", len(args))
		}
		row, ok := args[1].(deploy.ContainerRow)
		if !ok {
			t.Fatalf("hook arg[1] type = %T, want deploy.ContainerRow", args[1])
		}
		seenID = row.ID
		if row.ID == "c1" {
			return injected
		}
		return nil
	})

	err := storeA.InsertContainer(ctx, deploy.ContainerRow{ID: "c1", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "node-a", ContainerName: "ns-api-1"})
	if !errors.Is(err, injected) {
		t.Fatalf("InsertContainer(c1) error = %v, want injected", err)
	}
	if seenID != "c1" {
		t.Fatalf("hook seen ID = %q, want c1", seenID)
	}

	err = storeA.InsertContainer(ctx, deploy.ContainerRow{ID: "c2", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "node-a", ContainerName: "ns-api-2"})
	if err != nil {
		t.Fatalf("InsertContainer(c2) error = %v, want nil", err)
	}

	bRows, err := storeB.ListContainersByNamespace(ctx, "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace(node-b) error = %v", err)
	}
	if len(bRows) != 1 || bRows[0].ID != "c2" {
		t.Fatalf("node-b rows = %+v, want only c2", bRows)
	}
}

func TestClusterContainerStore_FaultPoints(t *testing.T) {
	baseRow := deploy.ContainerRow{
		ID:            "c1",
		Namespace:     "ns",
		DeployID:      "d1",
		Service:       "api",
		MachineID:     "node-a",
		ContainerName: "ns-api-1",
		CreatedAt:     "2025-01-01T00:00:00Z",
		UpdatedAt:     "2025-01-01T00:00:00Z",
	}

	tests := []struct {
		name  string
		point string
		setup func(*testing.T, *ClusterContainerStore)
		run   func(*testing.T, *ClusterContainerStore) error
	}{
		{
			name:  "ensure table",
			point: FaultClusterContainerStoreEnsureTable,
			run: func(t *testing.T, store *ClusterContainerStore) error {
				return store.EnsureContainerTable(t.Context())
			},
		},
		{
			name:  "insert",
			point: FaultClusterContainerStoreInsert,
			run: func(t *testing.T, store *ClusterContainerStore) error {
				return store.InsertContainer(t.Context(), baseRow)
			},
		},
		{
			name:  "update",
			point: FaultClusterContainerStoreUpdate,
			setup: func(t *testing.T, store *ClusterContainerStore) {
				if err := store.InsertContainer(t.Context(), baseRow); err != nil {
					t.Fatalf("setup InsertContainer() error = %v", err)
				}
			},
			run: func(t *testing.T, store *ClusterContainerStore) error {
				updated := baseRow
				updated.Status = "stopped"
				updated.UpdatedAt = "2025-01-01T00:01:00Z"
				return store.UpdateContainer(t.Context(), updated)
			},
		},
		{
			name:  "list by namespace",
			point: FaultClusterContainerStoreListByNamespace,
			setup: func(t *testing.T, store *ClusterContainerStore) {
				if err := store.InsertContainer(t.Context(), baseRow); err != nil {
					t.Fatalf("setup InsertContainer() error = %v", err)
				}
			},
			run: func(t *testing.T, store *ClusterContainerStore) error {
				rows, err := store.ListContainersByNamespace(t.Context(), "ns")
				if err != nil {
					return err
				}
				if len(rows) == 0 {
					return errors.New("ListContainersByNamespace() returned no rows")
				}
				return nil
			},
		},
		{
			name:  "list by deploy",
			point: FaultClusterContainerStoreListByDeploy,
			setup: func(t *testing.T, store *ClusterContainerStore) {
				if err := store.InsertContainer(t.Context(), baseRow); err != nil {
					t.Fatalf("setup InsertContainer() error = %v", err)
				}
			},
			run: func(t *testing.T, store *ClusterContainerStore) error {
				rows, err := store.ListContainersByDeploy(t.Context(), "ns", "d1")
				if err != nil {
					return err
				}
				if len(rows) == 0 {
					return errors.New("ListContainersByDeploy() returned no rows")
				}
				return nil
			},
		},
		{
			name:  "delete",
			point: FaultClusterContainerStoreDelete,
			setup: func(t *testing.T, store *ClusterContainerStore) {
				if err := store.InsertContainer(t.Context(), baseRow); err != nil {
					t.Fatalf("setup InsertContainer() error = %v", err)
				}
			},
			run: func(t *testing.T, store *ClusterContainerStore) error {
				return store.DeleteContainer(t.Context(), "c1")
			},
		},
		{
			name:  "delete by namespace",
			point: FaultClusterContainerStoreDeleteByNamespace,
			setup: func(t *testing.T, store *ClusterContainerStore) {
				if err := store.InsertContainer(t.Context(), baseRow); err != nil {
					t.Fatalf("setup InsertContainer() error = %v", err)
				}
			},
			run: func(t *testing.T, store *ClusterContainerStore) error {
				return store.DeleteContainersByNamespace(t.Context(), "ns")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
			cluster := NewCluster(clock)
			store := NewClusterContainerStore(cluster, "node-a")

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
