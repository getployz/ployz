package machine

import (
	"context"
	"errors"
	"slices"
	"testing"

	"ployz"
	"ployz/machine/mesh"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

type fakeWireGuard struct {
	calls   []string
	upErr   error
	downErr error
}

func (f *fakeWireGuard) Up(context.Context) error {
	f.calls = append(f.calls, "Up")
	return f.upErr
}

func (f *fakeWireGuard) SetPeers(context.Context, []ployz.MachineRecord) error {
	f.calls = append(f.calls, "SetPeers")
	return nil
}

func (f *fakeWireGuard) Down(context.Context) error {
	f.calls = append(f.calls, "Down")
	return f.downErr
}

type fakeStore struct {
	calls    []string
	startErr error
	stopErr  error
}

func (f *fakeStore) Start(context.Context) error {
	f.calls = append(f.calls, "Start")
	return f.startErr
}

func (f *fakeStore) Stop(context.Context) error {
	f.calls = append(f.calls, "Stop")
	return f.stopErr
}

func (f *fakeStore) ListMachines(context.Context) ([]ployz.MachineRecord, error) {
	return nil, nil
}

func (f *fakeStore) SubscribeMachines(context.Context) ([]ployz.MachineRecord, <-chan ployz.MachineEvent, error) {
	return nil, nil, nil
}

func (f *fakeStore) UpsertMachine(context.Context, ployz.MachineRecord) error { return nil }

func (f *fakeStore) DeleteMachine(context.Context, string) error { return nil }

func TestRun_StartupFailureDestroysPartialMesh(t *testing.T) {
	startErr := errors.New("store start failed")
	wg := &fakeWireGuard{}
	store := &fakeStore{startErr: startErr}

	m := newTestMachineWithNetworkConfig(t, mesh.New(mesh.WithWireGuard(wg), mesh.WithStore(store)))

	err := m.Run(context.Background())
	if err == nil {
		t.Fatal("Run should return an error")
	}
	if !errors.Is(err, startErr) {
		t.Fatalf("Run error = %v, want startup error", err)
	}

	if !slices.Equal(wg.calls, []string{"Up", "Down"}) {
		t.Fatalf("wireguard calls = %v, want [Up Down]", wg.calls)
	}
	if !slices.Equal(store.calls, []string{"Start", "Stop"}) {
		t.Fatalf("store calls = %v, want [Start Stop]", store.calls)
	}
}

func TestRun_StartupFailureIncludesDestroyError(t *testing.T) {
	startErr := errors.New("store start failed")
	destroyErr := errors.New("wg down failed")
	wg := &fakeWireGuard{downErr: destroyErr}
	store := &fakeStore{startErr: startErr}

	m := newTestMachineWithNetworkConfig(t, mesh.New(mesh.WithWireGuard(wg), mesh.WithStore(store)))

	err := m.Run(context.Background())
	if err == nil {
		t.Fatal("Run should return an error")
	}
	if !errors.Is(err, startErr) {
		t.Fatalf("Run error = %v, want startup error", err)
	}
	if !errors.Is(err, destroyErr) {
		t.Fatalf("Run error = %v, want destroy cleanup error", err)
	}

	if !slices.Equal(wg.calls, []string{"Up", "Down"}) {
		t.Fatalf("wireguard calls = %v, want [Up Down]", wg.calls)
	}
	if !slices.Equal(store.calls, []string{"Start", "Stop"}) {
		t.Fatalf("store calls = %v, want [Start Stop]", store.calls)
	}
}

func TestRun_StartupCanceledSkipsDestroy(t *testing.T) {
	wg := &fakeWireGuard{upErr: context.Canceled}
	m := newTestMachineWithNetworkConfig(t, mesh.New(mesh.WithWireGuard(wg)))

	err := m.Run(context.Background())
	if err == nil {
		t.Fatal("Run should return an error")
	}
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Run error = %v, want context canceled", err)
	}

	if !slices.Equal(wg.calls, []string{"Up"}) {
		t.Fatalf("wireguard calls = %v, want [Up]", wg.calls)
	}
}

func newTestMachineWithNetworkConfig(t *testing.T, msh *mesh.Mesh) *Machine {
	t.Helper()

	privateKey, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatalf("generate private key: %v", err)
	}

	m, err := New(
		t.TempDir(),
		WithIdentity(Identity{PrivateKey: privateKey, Name: "test-machine"}),
		WithMesh(msh),
	)
	if err != nil {
		t.Fatalf("create machine: %v", err)
	}

	if err := m.SaveNetworkConfig(NetworkConfig{Network: "test-network"}); err != nil {
		t.Fatalf("save network config: %v", err)
	}

	return m
}
