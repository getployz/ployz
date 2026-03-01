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

	m := newTestMachineWithNetworkConfig(t, net)

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

	m := newTestMachineWithNetworkConfig(t, net)

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

	m := newTestMachineWithNetworkConfig(t, net)

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

func newTestMachineWithNetworkConfig(t *testing.T, ns NetworkStack) *Machine {
	t.Helper()

	privateKey, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatalf("generate private key: %v", err)
	}

	m, err := New(
		t.TempDir(),
		WithIdentity(Identity{PrivateKey: privateKey, Name: "test-machine"}),
		WithMesh(ns),
	)
	if err != nil {
		t.Fatalf("create machine: %v", err)
	}

	if err := m.SaveNetworkConfig(NetworkConfig{Network: "test-network"}); err != nil {
		t.Fatalf("save network config: %v", err)
	}

	return m
}
