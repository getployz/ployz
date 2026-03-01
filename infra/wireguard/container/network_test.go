package container

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/containerd/errdefs"
)

func TestEnsureNetwork_ExistsIsNoop(t *testing.T) {
	docker := &fakeDocker{}

	if err := ensureNetwork(context.Background(), docker, "test-net"); err != nil {
		t.Fatalf("ensureNetwork: %v", err)
	}

	want := []string{"NetworkInspect"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestEnsureNetwork_CreatesWhenMissing(t *testing.T) {
	docker := &fakeDocker{networkInspectErr: errdefs.ErrNotFound}

	if err := ensureNetwork(context.Background(), docker, "test-net"); err != nil {
		t.Fatalf("ensureNetwork: %v", err)
	}

	want := []string{"NetworkInspect", "NetworkCreate"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestEnsureNetwork_WrapsInspectError(t *testing.T) {
	inspectErr := errors.New("docker unavailable")
	docker := &fakeDocker{networkInspectErr: inspectErr}

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
	docker := &fakeDocker{
		networkInspectErr: errdefs.ErrNotFound,
		networkCreateErr:  createErr,
	}

	err := ensureNetwork(context.Background(), docker, "test-net")
	if err == nil {
		t.Fatal("ensureNetwork should return error")
	}
	if !errors.Is(err, createErr) {
		t.Errorf("got %v, want wrapped %v", err, createErr)
	}
}
