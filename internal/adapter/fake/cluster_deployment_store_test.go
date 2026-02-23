package fake

import (
	"context"
	"testing"
	"time"

	"ployz/internal/deploy"
)

func TestClusterDeploymentStore_OwnershipReplicates(t *testing.T) {
	clock := NewClock(time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC))
	cluster := NewCluster(clock)

	storeA := NewClusterDeploymentStore(cluster, "node-a")
	storeB := NewClusterDeploymentStore(cluster, "node-b")

	ctx := context.Background()
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

	ctx := context.Background()
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
