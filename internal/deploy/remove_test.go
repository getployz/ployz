package deploy_test

import (
	"context"
	"errors"
	"testing"

	fakeleaf "ployz/internal/adapter/fake/leaf"
	"ployz/internal/deploy"
	"ployz/internal/mesh"
)

func TestRemoveNamespace_RemovesContainersAndRows(t *testing.T) {
	rt := fakeleaf.NewContainerRuntime()
	store := fakeleaf.NewContainerStore()

	ctx := context.Background()
	createAndStartContainer(t, rt, mesh.ContainerCreateConfig{
		Name:   "ns-api-1",
		Image:  "api:1",
		Labels: map[string]string{"ployz.namespace": "ns", "ployz.machine_id": "m1"},
	})
	createAndStartContainer(t, rt, mesh.ContainerCreateConfig{
		Name:   "ns-api-2",
		Image:  "api:1",
		Labels: map[string]string{"ployz.namespace": "ns", "ployz.machine_id": "m1"},
	})
	createAndStartContainer(t, rt, mesh.ContainerCreateConfig{
		Name:   "other-api-1",
		Image:  "api:1",
		Labels: map[string]string{"ployz.namespace": "other", "ployz.machine_id": "m1"},
	})

	_ = store.InsertContainer(ctx, deploy.ContainerRow{ID: "c1", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "m1", ContainerName: "ns-api-1"})
	_ = store.InsertContainer(ctx, deploy.ContainerRow{ID: "c2", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "m1", ContainerName: "ns-api-2"})
	_ = store.InsertContainer(ctx, deploy.ContainerRow{ID: "c3", Namespace: "other", DeployID: "d1", Service: "api", MachineID: "m1", ContainerName: "other-api-1"})

	err := deploy.RemoveNamespace(ctx, rt, deploy.Stores{Containers: store}, "ns", "m1")
	if err != nil {
		t.Fatalf("RemoveNamespace() error = %v", err)
	}

	if info, _ := rt.ContainerInspect(ctx, "ns-api-1"); info.Exists {
		t.Fatal("ns-api-1 still exists after RemoveNamespace")
	}
	if info, _ := rt.ContainerInspect(ctx, "ns-api-2"); info.Exists {
		t.Fatal("ns-api-2 still exists after RemoveNamespace")
	}
	if info, _ := rt.ContainerInspect(ctx, "other-api-1"); !info.Exists {
		t.Fatal("other-api-1 should remain")
	}

	nsRows, _ := store.ListContainersByNamespace(ctx, "ns")
	if len(nsRows) != 0 {
		t.Fatalf("ns rows = %+v, want empty", nsRows)
	}
	otherRows, _ := store.ListContainersByNamespace(ctx, "other")
	if len(otherRows) != 1 || otherRows[0].ID != "c3" {
		t.Fatalf("other rows = %+v, want c3", otherRows)
	}
}

func TestRemoveNamespace_ContinuesOnStopError(t *testing.T) {
	rt := fakeleaf.NewContainerRuntime()
	store := fakeleaf.NewContainerStore()

	ctx := context.Background()
	createAndStartContainer(t, rt, mesh.ContainerCreateConfig{
		Name:   "bad",
		Image:  "api:1",
		Labels: map[string]string{"ployz.namespace": "ns", "ployz.machine_id": "m1"},
	})
	createAndStartContainer(t, rt, mesh.ContainerCreateConfig{
		Name:   "good",
		Image:  "api:1",
		Labels: map[string]string{"ployz.namespace": "ns", "ployz.machine_id": "m1"},
	})

	_ = store.InsertContainer(ctx, deploy.ContainerRow{ID: "c1", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "m1", ContainerName: "bad"})
	_ = store.InsertContainer(ctx, deploy.ContainerRow{ID: "c2", Namespace: "ns", DeployID: "d1", Service: "api", MachineID: "m1", ContainerName: "good"})

	injected := errors.New("stop failed")
	rt.ContainerStopErr = func(ctx context.Context, name string) error {
		if name == "bad" {
			return injected
		}
		return nil
	}

	err := deploy.RemoveNamespace(ctx, rt, deploy.Stores{Containers: store}, "ns", "m1")
	if !errors.Is(err, injected) {
		t.Fatalf("RemoveNamespace() error = %v, want injected", err)
	}

	stops := rt.Calls("ContainerStop")
	if len(stops) != 2 {
		t.Fatalf("ContainerStop calls = %d, want 2", len(stops))
	}

	nsRows, _ := store.ListContainersByNamespace(ctx, "ns")
	if len(nsRows) != 0 {
		t.Fatalf("ns rows = %+v, want empty even on stop error", nsRows)
	}
}

func createAndStartContainer(t *testing.T, rt *fakeleaf.ContainerRuntime, cfg mesh.ContainerCreateConfig) {
	t.Helper()
	if err := rt.ContainerCreate(context.Background(), cfg); err != nil {
		t.Fatalf("ContainerCreate(%s) error = %v", cfg.Name, err)
	}
	if err := rt.ContainerStart(context.Background(), cfg.Name); err != nil {
		t.Fatalf("ContainerStart(%s) error = %v", cfg.Name, err)
	}
}
