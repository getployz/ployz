package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/deploy"
)

func TestContainerStore_InsertListUpdateDelete(t *testing.T) {
	ctx := t.Context()
	store := NewContainerStore()

	row := deploy.ContainerRow{
		ID:            "c1",
		Namespace:     "frontend",
		DeployID:      "d1",
		Service:       "api",
		MachineID:     "m1",
		ContainerName: "ployz-frontend-api-aaaa",
		SpecJSON:      `{"image":"api:1"}`,
		Status:        "running",
		Version:       1,
		CreatedAt:     "2026-01-01T00:00:00Z",
		UpdatedAt:     "2026-01-01T00:00:00Z",
	}

	if err := store.InsertContainer(ctx, row); err != nil {
		t.Fatalf("InsertContainer() error = %v", err)
	}

	list, err := store.ListContainersByNamespace(ctx, "frontend")
	if err != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", err)
	}
	if len(list) != 1 || list[0].ID != "c1" {
		t.Fatalf("ListContainersByNamespace() = %+v, want one row c1", list)
	}

	updated := row
	updated.Status = "stopped"
	updated.UpdatedAt = "2026-01-01T00:01:00Z"
	if err := store.UpdateContainer(ctx, updated); err != nil {
		t.Fatalf("UpdateContainer() error = %v", err)
	}

	byDeploy, err := store.ListContainersByDeploy(ctx, "frontend", "d1")
	if err != nil {
		t.Fatalf("ListContainersByDeploy() error = %v", err)
	}
	if len(byDeploy) != 1 {
		t.Fatalf("ListContainersByDeploy() len = %d, want 1", len(byDeploy))
	}
	if byDeploy[0].Status != "stopped" {
		t.Fatalf("updated status = %q, want %q", byDeploy[0].Status, "stopped")
	}
	if byDeploy[0].Version != 2 {
		t.Fatalf("updated version = %d, want 2", byDeploy[0].Version)
	}

	if err := store.DeleteContainer(ctx, "c1"); err != nil {
		t.Fatalf("DeleteContainer() error = %v", err)
	}
	list, err = store.ListContainersByNamespace(ctx, "frontend")
	if err != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", err)
	}
	if len(list) != 0 {
		t.Fatalf("after delete len = %d, want 0", len(list))
	}
}

func TestContainerStore_DeleteByNamespace(t *testing.T) {
	ctx := t.Context()
	store := NewContainerStore()

	_ = store.InsertContainer(ctx, deploy.ContainerRow{ID: "a", Namespace: "ns1", DeployID: "d", Service: "svc", MachineID: "m", ContainerName: "a"})
	_ = store.InsertContainer(ctx, deploy.ContainerRow{ID: "b", Namespace: "ns1", DeployID: "d", Service: "svc", MachineID: "m", ContainerName: "b"})
	_ = store.InsertContainer(ctx, deploy.ContainerRow{ID: "c", Namespace: "ns2", DeployID: "d", Service: "svc", MachineID: "m", ContainerName: "c"})

	if err := store.DeleteContainersByNamespace(ctx, "ns1"); err != nil {
		t.Fatalf("DeleteContainersByNamespace() error = %v", err)
	}

	ns1, _ := store.ListContainersByNamespace(ctx, "ns1")
	ns2, _ := store.ListContainersByNamespace(ctx, "ns2")
	if len(ns1) != 0 {
		t.Fatalf("ns1 len = %d, want 0", len(ns1))
	}
	if len(ns2) != 1 || ns2[0].ID != "c" {
		t.Fatalf("ns2 = %+v, want row c", ns2)
	}
}

func TestContainerStore_ErrorInjection(t *testing.T) {
	ctx := t.Context()
	store := NewContainerStore()
	injected := errors.New("injected")

	store.InsertContainerErr = func(ctx context.Context, row deploy.ContainerRow) error { return injected }
	if err := store.InsertContainer(ctx, deploy.ContainerRow{ID: "c1"}); !errors.Is(err, injected) {
		t.Fatalf("InsertContainer() error = %v, want injected", err)
	}

	store.ListByNamespaceErr = func(ctx context.Context, namespace string) error { return injected }
	if _, err := store.ListContainersByNamespace(ctx, "ns"); !errors.Is(err, injected) {
		t.Fatalf("ListContainersByNamespace() error = %v, want injected", err)
	}
}

func TestContainerStore_FaultFailOnce(t *testing.T) {
	ctx := t.Context()
	store := NewContainerStore()
	injected := errors.New("injected")
	store.FailOnce(FaultContainerStoreInsert, injected)

	row := deploy.ContainerRow{ID: "c1", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "m1", ContainerName: "c1"}
	err := store.InsertContainer(ctx, row)
	if !errors.Is(err, injected) {
		t.Fatalf("first InsertContainer() error = %v, want injected", err)
	}

	err = store.InsertContainer(ctx, row)
	if err != nil {
		t.Fatalf("second InsertContainer() error = %v, want nil", err)
	}

	list, err := store.ListContainersByNamespace(ctx, "ns")
	if err != nil {
		t.Fatalf("ListContainersByNamespace() error = %v", err)
	}
	if len(list) != 1 || list[0].ID != "c1" {
		t.Fatalf("ListContainersByNamespace() = %+v, want row c1", list)
	}
}
