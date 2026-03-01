package corrorun

import (
	"context"
	"errors"
	"net/netip"
	"slices"
	"testing"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
)

// fakeDocker records calls and returns configured responses.
// Embeds client.APIClient so unused methods panic if called.
type fakeDocker struct {
	client.APIClient

	inspectResult types.ContainerJSON
	inspectErr    error
	startErr      error
	createErr     error

	calls []string
}

func (f *fakeDocker) ContainerInspect(_ context.Context, _ string) (types.ContainerJSON, error) {
	f.calls = append(f.calls, "Inspect")
	return f.inspectResult, f.inspectErr
}

func (f *fakeDocker) ContainerStart(_ context.Context, _ string, _ container.StartOptions) error {
	f.calls = append(f.calls, "Start")
	return f.startErr
}

func (f *fakeDocker) ContainerCreate(_ context.Context, _ *container.Config, _ *container.HostConfig, _ *network.NetworkingConfig, _ *ocispec.Platform, _ string) (container.CreateResponse, error) {
	f.calls = append(f.calls, "Create")
	return container.CreateResponse{}, f.createErr
}

func noopReady(_ context.Context, _ netip.AddrPort) error { return nil }

func newTestContainer(docker *fakeDocker) *Container {
	return NewContainer(
		docker,
		Paths{Dir: "/tmp/test"},
		netip.MustParseAddrPort("127.0.0.1:9090"),
		WithReadinessCheck(noopReady),
	)
}

func TestStart_ReusesRunningContainer(t *testing.T) {
	docker := &fakeDocker{
		inspectResult: types.ContainerJSON{
			ContainerJSONBase: &types.ContainerJSONBase{
				State: &types.ContainerState{Running: true},
			},
		},
	}
	c := newTestContainer(docker)

	if err := c.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	// Should inspect only — no create, no start.
	want := []string{"Inspect"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestStart_StartsExistingStoppedContainer(t *testing.T) {
	docker := &fakeDocker{
		inspectResult: types.ContainerJSON{
			ContainerJSONBase: &types.ContainerJSONBase{
				State: &types.ContainerState{Running: false},
			},
		},
	}
	c := newTestContainer(docker)

	if err := c.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	// Should inspect then start — no create.
	want := []string{"Inspect", "Start"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestStart_CreatesWhenMissing(t *testing.T) {
	docker := &fakeDocker{
		inspectErr: errdefs.ErrNotFound,
	}
	c := newTestContainer(docker)

	if err := c.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	// Should inspect (not found) → create → start.
	want := []string{"Inspect", "Create", "Start"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestStart_WrapsInspectError(t *testing.T) {
	inspectErr := errors.New("docker daemon unreachable")
	docker := &fakeDocker{
		inspectErr: inspectErr,
	}
	c := newTestContainer(docker)

	err := c.Start(context.Background())
	if err == nil {
		t.Fatal("Start should return an error")
	}
	if !errors.Is(err, inspectErr) {
		t.Errorf("got %v, want wrapped %v", err, inspectErr)
	}

	// Should only inspect — no further calls on unexpected error.
	want := []string{"Inspect"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

