package corrorun

import (
	"context"
	"errors"
	"io"
	"net/netip"
	"slices"
	"strings"
	"testing"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/network"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
)

// fakeDocker records calls and returns configured responses.
type fakeDocker struct {
	inspectResult container.InspectResponse
	inspectErr    error
	startErr      error
	createErr     error

	calls []string
}

func (f *fakeDocker) ContainerInspect(_ context.Context, _ string) (container.InspectResponse, error) {
	f.calls = append(f.calls, "Inspect")
	return f.inspectResult, f.inspectErr
}

func (f *fakeDocker) ContainerCreate(_ context.Context, _ *container.Config, _ *container.HostConfig, _ *network.NetworkingConfig, _ *ocispec.Platform, _ string) (container.CreateResponse, error) {
	f.calls = append(f.calls, "Create")
	return container.CreateResponse{}, f.createErr
}

func (f *fakeDocker) ContainerStart(_ context.Context, _ string, _ container.StartOptions) error {
	f.calls = append(f.calls, "Start")
	return f.startErr
}

func (f *fakeDocker) ContainerStop(_ context.Context, _ string, _ container.StopOptions) error {
	f.calls = append(f.calls, "Stop")
	return nil
}

func (f *fakeDocker) ContainerRemove(_ context.Context, _ string, _ container.RemoveOptions) error {
	f.calls = append(f.calls, "Remove")
	return nil
}

func (f *fakeDocker) ImagePull(_ context.Context, _ string, _ image.PullOptions) (io.ReadCloser, error) {
	f.calls = append(f.calls, "Pull")
	return io.NopCloser(strings.NewReader("")), nil
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

func runningContainer() container.InspectResponse {
	return container.InspectResponse{
		ContainerJSONBase: &container.ContainerJSONBase{
			State: &container.State{Running: true},
		},
	}
}

func stoppedContainer() container.InspectResponse {
	return container.InspectResponse{
		ContainerJSONBase: &container.ContainerJSONBase{
			State: &container.State{Running: false},
		},
	}
}

func TestStart_ReusesRunningContainer(t *testing.T) {
	docker := &fakeDocker{inspectResult: runningContainer()}
	c := newTestContainer(docker)

	if err := c.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	want := []string{"Inspect"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestStart_StartsExistingStoppedContainer(t *testing.T) {
	docker := &fakeDocker{inspectResult: stoppedContainer()}
	c := newTestContainer(docker)

	if err := c.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	want := []string{"Inspect", "Start"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestStart_CreatesWhenMissing(t *testing.T) {
	docker := &fakeDocker{inspectErr: errdefs.ErrNotFound}
	c := newTestContainer(docker)

	if err := c.Start(context.Background()); err != nil {
		t.Fatalf("Start: %v", err)
	}

	want := []string{"Inspect", "Create", "Start"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestStart_WrapsInspectError(t *testing.T) {
	inspectErr := errors.New("docker daemon unreachable")
	docker := &fakeDocker{inspectErr: inspectErr}
	c := newTestContainer(docker)

	err := c.Start(context.Background())
	if err == nil {
		t.Fatal("Start should return an error")
	}
	if !errors.Is(err, inspectErr) {
		t.Errorf("got %v, want wrapped %v", err, inspectErr)
	}

	want := []string{"Inspect"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}
