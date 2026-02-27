package manager

import (
	"context"
	"errors"
	"net/netip"
	"os"
	"testing"

	"ployz/internal/engine"
	"ployz/internal/network"
	"ployz/pkg/sdk/types"
)

func TestDisableNetworkBlocksWhenManagedWorkloadsExist(t *testing.T) {
	t.Parallel()

	store := &fakeSpecStore{
		spec: PersistedSpec{Spec: types.NetworkSpec{Network: "testnet"}, Enabled: true},
		ok:   true,
	}
	runtime := &fakeContainerRuntime{
		entries: []network.ContainerListEntry{
			{Name: "web-1", Labels: map[string]string{managedLabelNamespace: "shop", managedLabelDeployID: "deploy-123"}},
			{Name: "corrosion", Labels: map[string]string{}},
		},
	}

	mgr := newTestManager(t, store, runtime, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	}, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	})
	err := mgr.DisableNetwork(context.Background(), false)
	if !errors.Is(err, ErrNetworkDestroyHasWorkloads) {
		t.Fatalf("expected ErrNetworkDestroyHasWorkloads, got %v", err)
	}
	if err.Error() != "network destroy blocked by managed workloads: local runtime has 1 managed workload containers (shop)" {
		t.Fatalf("expected workload count in error, got %v", err)
	}
	if len(store.saveEnabled) != 0 {
		t.Fatalf("expected no SaveSpec call on blocked destroy, got %d", len(store.saveEnabled))
	}
}

func TestDisableNetworkBlocksWhenControlPlaneWorkloadsExist(t *testing.T) {
	t.Parallel()

	store := &fakeSpecStore{
		spec: PersistedSpec{Spec: types.NetworkSpec{Network: "testnet"}, Enabled: true},
		ok:   true,
	}
	runtime := &fakeContainerRuntime{}

	mgr := newTestManager(t, store, runtime, func(context.Context, network.Config) (int, []string, error) {
		return 2, []string{"shop", "payments"}, nil
	}, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	})
	err := mgr.DisableNetwork(context.Background(), false)
	if !errors.Is(err, ErrNetworkDestroyHasWorkloads) {
		t.Fatalf("expected ErrNetworkDestroyHasWorkloads, got %v", err)
	}
	if err.Error() != "network destroy blocked by managed workloads: control-plane has 2 managed workload containers (payments, shop)" {
		t.Fatalf("expected control-plane workload detail, got %v", err)
	}
	if len(store.saveEnabled) != 0 {
		t.Fatalf("expected no SaveSpec call on blocked destroy, got %d", len(store.saveEnabled))
	}
}

func TestDisableNetworkSucceedsWithoutManagedWorkloads(t *testing.T) {
	t.Parallel()

	store := &fakeSpecStore{
		spec: PersistedSpec{Spec: types.NetworkSpec{Network: "testnet"}, Enabled: true},
		ok:   true,
	}
	runtime := &fakeContainerRuntime{
		entries: []network.ContainerListEntry{{
			Name:   "helper",
			Labels: map[string]string{managedLabelNamespace: "shop"},
		}},
	}

	mgr := newTestManager(t, store, runtime, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	}, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	})
	if err := mgr.DisableNetwork(context.Background(), false); err != nil {
		t.Fatalf("disable network: %v", err)
	}
	if len(store.saveEnabled) != 1 {
		t.Fatalf("expected one SaveSpec call, got %d", len(store.saveEnabled))
	}
	if store.saveEnabled[0] {
		t.Fatalf("expected SaveSpec enabled=false")
	}
}

func TestDisableNetworkBlocksWhenAttachedMachinesExist(t *testing.T) {
	t.Parallel()

	store := &fakeSpecStore{
		spec: PersistedSpec{Spec: types.NetworkSpec{Network: "testnet"}, Enabled: true},
		ok:   true,
	}
	runtime := &fakeContainerRuntime{}

	mgr := newTestManager(t, store, runtime, func(context.Context, network.Config) (int, []string, error) {
		return 0, nil, nil
	}, func(context.Context, network.Config) (int, []string, error) {
		return 2, []string{"machine-b", "machine-a"}, nil
	})
	err := mgr.DisableNetwork(context.Background(), false)
	if !errors.Is(err, ErrNetworkDestroyHasMachines) {
		t.Fatalf("expected ErrNetworkDestroyHasMachines, got %v", err)
	}
	if err.Error() != "network destroy blocked by attached machines: network has 2 attached machines (machine-a, machine-b)" {
		t.Fatalf("expected attached machine detail, got %v", err)
	}
	if len(store.saveEnabled) != 0 {
		t.Fatalf("expected no SaveSpec call on blocked destroy, got %d", len(store.saveEnabled))
	}
}

func newTestManager(
	t *testing.T,
	specStore *fakeSpecStore,
	runtime *fakeContainerRuntime,
	controlPlaneSummary ControlPlaneWorkloadSummaryFunc,
	attachedSummary AttachedMachinesSummaryFunc,
) *Manager {
	t.Helper()

	ctrl, err := network.New(
		network.WithContainerRuntime(runtime),
		network.WithCorrosionRuntime(fakeCorrosionRuntime{}),
		network.WithStatusProber(fakeStatusProber{}),
		network.WithPlatformOps(fakePlatformOps{}),
		network.WithRegistryFactory(func(netip.AddrPort, string) network.Registry { return nil }),
		network.WithStateStore(fakeStateStore{}),
		network.WithClock(network.RealClock{}),
	)
	if err != nil {
		t.Fatalf("build test controller: %v", err)
	}

	return &Manager{
		ctx:                         context.Background(),
		dataRoot:                    t.TempDir(),
		store:                       specStore,
		ctrl:                        ctrl,
		engine:                      &engine.Engine{},
		controlPlaneWorkloadSummary: controlPlaneSummary,
		attachedMachinesSummary:     attachedSummary,
	}
}

type fakeSpecStore struct {
	spec        PersistedSpec
	ok          bool
	getErr      error
	saveErr     error
	deleteErr   error
	saveEnabled []bool
	deleted     bool
}

func (s *fakeSpecStore) SaveSpec(spec types.NetworkSpec, enabled bool) error {
	if s.saveErr != nil {
		return s.saveErr
	}
	s.spec = PersistedSpec{Spec: spec, Enabled: enabled}
	s.ok = true
	s.saveEnabled = append(s.saveEnabled, enabled)
	return nil
}

func (s *fakeSpecStore) GetSpec() (PersistedSpec, bool, error) {
	if s.getErr != nil {
		return PersistedSpec{}, false, s.getErr
	}
	return s.spec, s.ok, nil
}

func (s *fakeSpecStore) DeleteSpec() error {
	if s.deleteErr != nil {
		return s.deleteErr
	}
	s.deleted = true
	s.ok = false
	s.spec = PersistedSpec{}
	return nil
}

func (s *fakeSpecStore) Close() error { return nil }

type fakeContainerRuntime struct {
	entries []network.ContainerListEntry
	listErr error
}

func (f *fakeContainerRuntime) WaitReady(context.Context) error { return nil }

func (f *fakeContainerRuntime) ContainerInspect(context.Context, string) (network.ContainerInfo, error) {
	return network.ContainerInfo{}, nil
}

func (f *fakeContainerRuntime) ContainerStart(context.Context, string) error { return nil }

func (f *fakeContainerRuntime) ContainerStop(context.Context, string) error { return nil }

func (f *fakeContainerRuntime) ContainerRemove(context.Context, string, bool) error { return nil }

func (f *fakeContainerRuntime) ContainerLogs(context.Context, string, int) (string, error) {
	return "", nil
}

func (f *fakeContainerRuntime) ContainerCreate(context.Context, network.ContainerCreateConfig) error {
	return nil
}

func (f *fakeContainerRuntime) ContainerList(context.Context, map[string]string) ([]network.ContainerListEntry, error) {
	if f.listErr != nil {
		return nil, f.listErr
	}
	return append([]network.ContainerListEntry(nil), f.entries...), nil
}

func (f *fakeContainerRuntime) ContainerUpdate(context.Context, string, network.ResourceConfig) error {
	return nil
}

func (f *fakeContainerRuntime) ImagePull(context.Context, string) error { return nil }

func (f *fakeContainerRuntime) NetworkInspect(context.Context, string) (network.NetworkInfo, error) {
	return network.NetworkInfo{}, nil
}

func (f *fakeContainerRuntime) NetworkCreate(context.Context, string, netip.Prefix, string) error {
	return nil
}

func (f *fakeContainerRuntime) NetworkRemove(context.Context, string) error { return nil }

func (f *fakeContainerRuntime) Close() error { return nil }

type fakeStateStore struct{}

func (fakeStateStore) Load(string) (*network.State, error) { return nil, os.ErrNotExist }

func (fakeStateStore) Save(string, *network.State) error { return nil }

func (fakeStateStore) Delete(string) error { return nil }

func (fakeStateStore) StatePath(string) string { return "" }

type fakeCorrosionRuntime struct{}

func (fakeCorrosionRuntime) WriteConfig(network.CorrosionConfig) error { return nil }

func (fakeCorrosionRuntime) Start(context.Context, network.CorrosionConfig) error { return nil }

func (fakeCorrosionRuntime) Stop(context.Context, string) error { return nil }

type fakeStatusProber struct{}

func (fakeStatusProber) ProbeInfra(context.Context, *network.State) (bool, bool, bool, error) {
	return true, true, true, nil
}

type fakePlatformOps struct{}

func (fakePlatformOps) Prepare(context.Context, network.Config, network.StateStore) error { return nil }

func (fakePlatformOps) ConfigureWireGuard(context.Context, network.Config, *network.State) error {
	return nil
}

func (fakePlatformOps) EnsureDockerNetwork(context.Context, network.Config, *network.State) error {
	return nil
}

func (fakePlatformOps) CleanupDockerNetwork(context.Context, network.Config, *network.State) error {
	return nil
}

func (fakePlatformOps) CleanupWireGuard(context.Context, network.Config, *network.State) error {
	return nil
}

func (fakePlatformOps) AfterStart(context.Context, network.Config) error { return nil }

func (fakePlatformOps) AfterStop(context.Context, network.Config, *network.State) error { return nil }

func (fakePlatformOps) ApplyPeerConfig(context.Context, network.Config, *network.State, []network.Peer) error {
	return nil
}
