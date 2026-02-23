package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/deploy"
)

func TestDeploymentStore_CRUDAndQueries(t *testing.T) {
	ctx := t.Context()
	store := NewDeploymentStore()

	row1 := deploy.DeploymentRow{
		ID:        "d1",
		Namespace: "frontend",
		Status:    "in_progress",
		CreatedAt: "2026-01-01T00:00:00Z",
		UpdatedAt: "2026-01-01T00:00:00Z",
	}
	row2 := deploy.DeploymentRow{
		ID:        "d2",
		Namespace: "frontend",
		Status:    "succeeded",
		CreatedAt: "2026-01-01T00:01:00Z",
		UpdatedAt: "2026-01-01T00:01:00Z",
	}
	row3 := deploy.DeploymentRow{
		ID:        "d3",
		Namespace: "frontend",
		Status:    "succeeded",
		CreatedAt: "2026-01-01T00:02:00Z",
		UpdatedAt: "2026-01-01T00:02:00Z",
	}

	if err := store.InsertDeployment(ctx, row1); err != nil {
		t.Fatalf("InsertDeployment(d1) error = %v", err)
	}
	if err := store.InsertDeployment(ctx, row2); err != nil {
		t.Fatalf("InsertDeployment(d2) error = %v", err)
	}
	if err := store.InsertDeployment(ctx, row3); err != nil {
		t.Fatalf("InsertDeployment(d3) error = %v", err)
	}

	got, ok, err := store.GetDeployment(ctx, "d1")
	if err != nil || !ok {
		t.Fatalf("GetDeployment(d1) ok=%v err=%v", ok, err)
	}
	if got.ID != "d1" {
		t.Fatalf("GetDeployment(d1).ID = %q, want d1", got.ID)
	}

	active, ok, err := store.GetActiveDeployment(ctx, "frontend")
	if err != nil || !ok {
		t.Fatalf("GetActiveDeployment() ok=%v err=%v", ok, err)
	}
	if active.ID != "d1" {
		t.Fatalf("active.ID = %q, want d1", active.ID)
	}

	latest, ok, err := store.LatestSuccessful(ctx, "frontend")
	if err != nil || !ok {
		t.Fatalf("LatestSuccessful() ok=%v err=%v", ok, err)
	}
	if latest.ID != "d3" {
		t.Fatalf("latest successful ID = %q, want d3", latest.ID)
	}

	list, err := store.ListByNamespace(ctx, "frontend")
	if err != nil {
		t.Fatalf("ListByNamespace() error = %v", err)
	}
	if len(list) != 3 {
		t.Fatalf("ListByNamespace() len = %d, want 3", len(list))
	}

	if err := store.DeleteDeployment(ctx, "d2"); err != nil {
		t.Fatalf("DeleteDeployment() error = %v", err)
	}
	list, err = store.ListByNamespace(ctx, "frontend")
	if err != nil {
		t.Fatalf("ListByNamespace() error = %v", err)
	}
	if len(list) != 2 {
		t.Fatalf("after delete len = %d, want 2", len(list))
	}
}

func TestDeploymentStore_Ownership(t *testing.T) {
	ctx := t.Context()
	store := NewDeploymentStore()

	row := deploy.DeploymentRow{ID: "d1", Namespace: "frontend", Status: "in_progress", CreatedAt: "2026-01-01T00:00:00Z"}
	if err := store.InsertDeployment(ctx, row); err != nil {
		t.Fatalf("InsertDeployment() error = %v", err)
	}

	if err := store.AcquireOwnership(ctx, "d1", "m1", "2026-01-01T00:00:01Z"); err != nil {
		t.Fatalf("AcquireOwnership(m1) error = %v", err)
	}

	if err := store.CheckOwnership(ctx, "d1", "m1"); err != nil {
		t.Fatalf("CheckOwnership(m1) error = %v", err)
	}

	if err := store.AcquireOwnership(ctx, "d1", "m1", "2026-01-01T00:00:02Z"); err != nil {
		t.Fatalf("AcquireOwnership(m1) idempotent error = %v", err)
	}

	if err := store.AcquireOwnership(ctx, "d1", "m2", "2026-01-01T00:00:03Z"); err == nil {
		t.Fatal("AcquireOwnership(m2) expected contention error")
	}

	if err := store.CheckOwnership(ctx, "d1", "m2"); err == nil {
		t.Fatal("CheckOwnership(m2) expected contention error")
	}

	if err := store.BumpOwnershipHeartbeat(ctx, "d1", "m1", "2026-01-01T00:00:04Z"); err != nil {
		t.Fatalf("BumpOwnershipHeartbeat() error = %v", err)
	}

	got, ok, err := store.GetDeployment(ctx, "d1")
	if err != nil || !ok {
		t.Fatalf("GetDeployment(d1) ok=%v err=%v", ok, err)
	}
	if got.OwnerHeartbeat != "2026-01-01T00:00:04Z" {
		t.Fatalf("OwnerHeartbeat = %q, want %q", got.OwnerHeartbeat, "2026-01-01T00:00:04Z")
	}

	if err := store.ReleaseOwnership(ctx, "d1"); err != nil {
		t.Fatalf("ReleaseOwnership() error = %v", err)
	}
	got, ok, err = store.GetDeployment(ctx, "d1")
	if err != nil || !ok {
		t.Fatalf("GetDeployment(d1) ok=%v err=%v", ok, err)
	}
	if got.Owner != "" || got.OwnerHeartbeat != "" {
		t.Fatalf("ownership not cleared: owner=%q heartbeat=%q", got.Owner, got.OwnerHeartbeat)
	}
}

func TestDeploymentStore_ErrorInjection(t *testing.T) {
	ctx := t.Context()
	store := NewDeploymentStore()
	injected := errors.New("injected")

	store.InsertDeploymentErr = func(ctx context.Context, row deploy.DeploymentRow) error { return injected }
	if err := store.InsertDeployment(ctx, deploy.DeploymentRow{ID: "d1"}); !errors.Is(err, injected) {
		t.Fatalf("InsertDeployment() error = %v, want injected", err)
	}

	store.AcquireOwnershipErr = func(ctx context.Context, deployID, machineID, now string) error { return injected }
	if err := store.AcquireOwnership(ctx, "d1", "m1", "now"); !errors.Is(err, injected) {
		t.Fatalf("AcquireOwnership() error = %v, want injected", err)
	}
}

func TestDeploymentStore_FaultFailOnce(t *testing.T) {
	ctx := t.Context()
	store := NewDeploymentStore()
	injected := errors.New("injected")
	store.FailOnce(FaultDeploymentStoreInsert, injected)

	row := deploy.DeploymentRow{ID: "d1", Namespace: "ns", Status: "in_progress", CreatedAt: "2026-01-01T00:00:00Z"}
	err := store.InsertDeployment(ctx, row)
	if !errors.Is(err, injected) {
		t.Fatalf("first InsertDeployment() error = %v, want injected", err)
	}

	err = store.InsertDeployment(ctx, row)
	if err != nil {
		t.Fatalf("second InsertDeployment() error = %v, want nil", err)
	}

	got, ok, err := store.GetDeployment(ctx, "d1")
	if err != nil || !ok {
		t.Fatalf("GetDeployment() ok=%v err=%v", ok, err)
	}
	if got.ID != "d1" {
		t.Fatalf("GetDeployment().ID = %q, want d1", got.ID)
	}
}
