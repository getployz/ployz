package supervisor_test

import (
	"context"
	"net/netip"
	"strings"
	"testing"

	clusterfake "ployz/internal/adapter/fake/cluster"
	leaffake "ployz/internal/adapter/fake/leaf"
	"ployz/internal/daemon/supervisor"
	"ployz/internal/engine"
	"ployz/internal/mesh"
	"ployz/internal/reconcile"
	"ployz/pkg/sdk/types"
)

func TestNewRequiresInjectedDependencies(t *testing.T) {
	_, err := supervisor.New(context.Background(), "/tmp/supervisor-missing-deps")
	if err == nil {
		t.Fatal("expected error when dependencies are not injected")
	}
	if !strings.Contains(err.Error(), "spec store is required") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestNewWithInjectedDependencies(t *testing.T) {
	ctx := t.Context()

	deps, err := newDeps(ctx, "node-a")
	if err != nil {
		t.Fatalf("create deps: %v", err)
	}

	mgr, err := supervisor.New(ctx, "/tmp/supervisor-injected",
		supervisor.WithSpecStore(deps.specStore),
		supervisor.WithManagerStateStore(deps.stateStore),
		supervisor.WithManagerController(deps.controller),
		supervisor.WithManagerEngine(deps.engine),
	)
	if err != nil {
		t.Fatalf("supervisor.New: %v", err)
	}
	if mgr == nil {
		t.Fatal("expected non-nil manager")
	}
}

func TestDisableNetworkPersistsDisabledSpec(t *testing.T) {
	ctx := t.Context()
	deps, err := newDeps(ctx, "node-disable")
	if err != nil {
		t.Fatalf("create deps: %v", err)
	}

	mgr, err := supervisor.New(ctx, "/tmp/supervisor-disable-persist",
		supervisor.WithSpecStore(deps.specStore),
		supervisor.WithManagerStateStore(deps.stateStore),
		supervisor.WithManagerController(deps.controller),
		supervisor.WithManagerEngine(deps.engine),
	)
	if err != nil {
		t.Fatalf("supervisor.New: %v", err)
	}

	if _, err := mgr.ApplyNetworkSpec(ctx, types.NetworkSpec{Network: "default", DataRoot: "/tmp/supervisor-disable-persist"}); err != nil {
		t.Fatalf("ApplyNetworkSpec: %v", err)
	}

	if err := mgr.DisableNetwork(ctx, "default", false); err != nil {
		t.Fatalf("DisableNetwork: %v", err)
	}

	persisted, ok, err := deps.specStore.GetSpec("default")
	if err != nil {
		t.Fatalf("GetSpec: %v", err)
	}
	if !ok {
		t.Fatal("expected spec to remain persisted")
	}
	if persisted.Enabled {
		t.Fatal("expected spec to be persisted as disabled")
	}
}

func TestDisableNetworkPurgeDeletesSpec(t *testing.T) {
	ctx := t.Context()
	deps, err := newDeps(ctx, "node-purge")
	if err != nil {
		t.Fatalf("create deps: %v", err)
	}

	mgr, err := supervisor.New(ctx, "/tmp/supervisor-disable-purge",
		supervisor.WithSpecStore(deps.specStore),
		supervisor.WithManagerStateStore(deps.stateStore),
		supervisor.WithManagerController(deps.controller),
		supervisor.WithManagerEngine(deps.engine),
	)
	if err != nil {
		t.Fatalf("supervisor.New: %v", err)
	}

	if _, err := mgr.ApplyNetworkSpec(ctx, types.NetworkSpec{Network: "default", DataRoot: "/tmp/supervisor-disable-purge"}); err != nil {
		t.Fatalf("ApplyNetworkSpec: %v", err)
	}

	if err := mgr.DisableNetwork(ctx, "default", true); err != nil {
		t.Fatalf("DisableNetwork purge: %v", err)
	}

	_, ok, err := deps.specStore.GetSpec("default")
	if err != nil {
		t.Fatalf("GetSpec: %v", err)
	}
	if ok {
		t.Fatal("expected spec to be deleted when purge=true")
	}
}

type testDeps struct {
	specStore  *leaffake.SpecStore
	stateStore *leaffake.StateStore
	controller *mesh.Controller
	engine     *engine.Engine
}

func newDeps(ctx context.Context, nodeID string) (testDeps, error) {
	cluster := clusterfake.NewCluster(mesh.RealClock{})
	stateStore := leaffake.NewStateStore()
	specStore := leaffake.NewSpecStore()
	platformOps := &leaffake.PlatformOps{}
	containerRT := leaffake.NewContainerRuntime()
	corrosionRT := leaffake.NewCorrosionRuntime()
	statusProber := &leaffake.StatusProber{WG: true, DockerNet: true, Corrosion: true}

	newController := func(opts ...mesh.Option) (*mesh.Controller, error) {
		allOpts := []mesh.Option{
			mesh.WithStateStore(stateStore),
			mesh.WithPlatformOps(platformOps),
			mesh.WithContainerRuntime(containerRT),
			mesh.WithCorrosionRuntime(corrosionRT),
			mesh.WithStatusProber(statusProber),
			mesh.WithRegistryFactory(cluster.NetworkRegistryFactory(nodeID)),
			mesh.WithClock(mesh.RealClock{}),
		}
		allOpts = append(allOpts, opts...)
		return mesh.New(allOpts...)
	}

	ctrl, err := newController()
	if err != nil {
		return testDeps{}, err
	}

	eng := engine.New(ctx,
		engine.WithControllerFactory(func() (engine.NetworkController, error) {
			return newController()
		}),
		engine.WithPeerReconcilerFactory(func() (reconcile.PeerReconciler, error) {
			return newController()
		}),
		engine.WithRegistryFactory(func(addr netip.AddrPort, token string) reconcile.Registry {
			return cluster.Registry(nodeID)
		}),
		engine.WithStateStore(stateStore),
	)

	return testDeps{
		specStore:  specStore,
		stateStore: stateStore,
		controller: ctrl,
		engine:     eng,
	}, nil
}
