package fake

import (
	"context"
	"testing"
	"time"

	"ployz/internal/deploy"
)

func TestClusterContainerStore_ReplicatesAcrossNodes(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)

	storeA := NewClusterContainerStore(cluster, "node-a")
	storeB := NewClusterContainerStore(cluster, "node-b")

	ctx := context.Background()
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

	ctx := context.Background()
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
