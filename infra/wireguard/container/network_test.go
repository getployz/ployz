package container

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
)

type fakeNetworkDocker struct {
	client.APIClient

	inspectErr error
	createErr  error
	calls      []string
}

func (f *fakeNetworkDocker) NetworkInspect(_ context.Context, _ string, _ network.InspectOptions) (network.Inspect, error) {
	f.calls = append(f.calls, "Inspect")
	return network.Inspect{}, f.inspectErr
}

func (f *fakeNetworkDocker) NetworkCreate(_ context.Context, _ string, _ network.CreateOptions) (network.CreateResponse, error) {
	f.calls = append(f.calls, "Create")
	return network.CreateResponse{}, f.createErr
}

func TestEnsureNetwork_ExistsIsNoop(t *testing.T) {
	docker := &fakeNetworkDocker{}

	if err := ensureNetwork(context.Background(), docker, "test-net"); err != nil {
		t.Fatalf("ensureNetwork: %v", err)
	}

	want := []string{"Inspect"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestEnsureNetwork_CreatesWhenMissing(t *testing.T) {
	docker := &fakeNetworkDocker{
		inspectErr: errdefs.ErrNotFound,
	}

	if err := ensureNetwork(context.Background(), docker, "test-net"); err != nil {
		t.Fatalf("ensureNetwork: %v", err)
	}

	want := []string{"Inspect", "Create"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestEnsureNetwork_WrapsInspectError(t *testing.T) {
	inspectErr := errors.New("docker unavailable")
	docker := &fakeNetworkDocker{
		inspectErr: inspectErr,
	}

	err := ensureNetwork(context.Background(), docker, "test-net")
	if err == nil {
		t.Fatal("ensureNetwork should return error")
	}
	if !errors.Is(err, inspectErr) {
		t.Errorf("got %v, want wrapped %v", err, inspectErr)
	}
}

func TestEnsureNetwork_WrapsCreateError(t *testing.T) {
	createErr := errors.New("network create failed")
	docker := &fakeNetworkDocker{
		inspectErr: errdefs.ErrNotFound,
		createErr:  createErr,
	}

	err := ensureNetwork(context.Background(), docker, "test-net")
	if err == nil {
		t.Fatal("ensureNetwork should return error")
	}
	if !errors.Is(err, createErr) {
		t.Errorf("got %v, want wrapped %v", err, createErr)
	}
}
