package machine

import (
	"context"
	"errors"
	"slices"
	"testing"

	"ployz/machine/mesh"

	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

type fakeNetwork struct {
	calls      []string
	upErr      error
	detachErr  error
	destroyErr error
	phase      mesh.Phase
	store      mesh.Store
}

func (f *fakeNetwork) Up(context.Context) error {
	f.calls = append(f.calls, "Up")
	return f.upErr
}

func (f *fakeNetwork) Detach(context.Context) error {
	f.calls = append(f.calls, "Detach")
	return f.detachErr
}

func (f *fakeNetwork) Destroy(context.Context) error {
	f.calls = append(f.calls, "Destroy")
	return f.destroyErr
}

func (f *fakeNetwork) Phase() mesh.Phase { return f.phase }
func (f *fakeNetwork) Store() mesh.Store { return f.store }

func TestRun_StartupFailureDestroysPartialMesh(t *testing.T) {
	startErr := errors.New("store start failed")
	net := &fakeNetwork{upErr: startErr}

	m := newTestMachineWithMesh(t, net)

	err := m.Run(context.Background())
	if err == nil {
		t.Fatal("Run should return an error")
	}
	if !errors.Is(err, startErr) {
		t.Fatalf("Run error = %v, want startup error", err)
	}

	if !slices.Equal(net.calls, []string{"Up", "Destroy"}) {
		t.Fatalf("network calls = %v, want [Up Destroy]", net.calls)
	}
}

func TestRun_StartupFailureIncludesDestroyError(t *testing.T) {
	startErr := errors.New("store start failed")
	destroyErr := errors.New("wg down failed")
	net := &fakeNetwork{upErr: startErr, destroyErr: destroyErr}

	m := newTestMachineWithMesh(t, net)

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

	if !slices.Equal(net.calls, []string{"Up", "Destroy"}) {
		t.Fatalf("network calls = %v, want [Up Destroy]", net.calls)
	}
}

func TestRun_StartupCanceledSkipsDestroy(t *testing.T) {
	net := &fakeNetwork{upErr: context.Canceled}

	m := newTestMachineWithMesh(t, net)

	err := m.Run(context.Background())
	if err == nil {
		t.Fatal("Run should return an error")
	}
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("Run error = %v, want context canceled", err)
	}

	if !slices.Equal(net.calls, []string{"Up"}) {
		t.Fatalf("network calls = %v, want [Up]", net.calls)
	}
}

func TestInitNetwork_NilNetworkStack(t *testing.T) {
	m := newTestMachine(t)

	err := m.InitNetwork(context.Background(), "test-net", nil)
	if err == nil {
		t.Fatal("InitNetwork should return error for nil network stack")
	}

	// No config should be saved.
	has, _ := m.HasNetworkConfig()
	if has {
		t.Fatal("network config should not exist after nil ns error")
	}
}

func TestInitNetwork_AlreadyRunning(t *testing.T) {
	net := &fakeNetwork{}
	m := newTestMachineWithMesh(t, net)

	err := m.InitNetwork(context.Background(), "test-net", &fakeNetwork{})
	if err == nil {
		t.Fatal("InitNetwork should return error when mesh already attached")
	}

	if len(net.calls) != 0 {
		t.Fatalf("existing mesh should not be touched, got calls: %v", net.calls)
	}
}

func TestInitNetwork_Success(t *testing.T) {
	m := newTestMachine(t)
	net := &fakeNetwork{}

	if err := m.InitNetwork(context.Background(), "test-net", net); err != nil {
		t.Fatalf("InitNetwork failed: %v", err)
	}

	if !slices.Equal(net.calls, []string{"Up"}) {
		t.Fatalf("network calls = %v, want [Up]", net.calls)
	}

	has, err := m.HasNetworkConfig()
	if err != nil {
		t.Fatalf("HasNetworkConfig: %v", err)
	}
	if !has {
		t.Fatal("network config should exist after successful init")
	}

	if !m.HasMeshAttached() {
		t.Fatal("mesh should be attached after successful init")
	}
}

func TestInitNetwork_StartFailure_CleansUp(t *testing.T) {
	m := newTestMachine(t)
	startErr := errors.New("wg up failed")
	net := &fakeNetwork{upErr: startErr}

	err := m.InitNetwork(context.Background(), "test-net", net)
	if err == nil {
		t.Fatal("InitNetwork should return error on start failure")
	}
	if !errors.Is(err, startErr) {
		t.Fatalf("InitNetwork error = %v, want %v", err, startErr)
	}

	// Config should be cleaned up.
	has, _ := m.HasNetworkConfig()
	if has {
		t.Fatal("network config should be removed after start failure")
	}

	// Mesh should be nil'd.
	if m.HasMeshAttached() {
		t.Fatal("mesh should not be attached after start failure")
	}
}

func newTestMachine(t *testing.T) *Machine {
	t.Helper()

	privateKey, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatalf("generate private key: %v", err)
	}

	m, err := New(
		t.TempDir(),
		WithIdentity(Identity{PrivateKey: privateKey, Name: "test-machine"}),
	)
	if err != nil {
		t.Fatalf("create machine: %v", err)
	}
	return m
}

func newTestMachineWithMesh(t *testing.T, ns NetworkStack) *Machine {
	t.Helper()

	m := newTestMachine(t)
	m.SetMesh(ns)
	return m
}
