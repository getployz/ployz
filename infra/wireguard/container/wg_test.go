package container

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// fakeDocker records calls and returns configured responses.
type fakeDocker struct {
	client.APIClient

	inspectResult types.ContainerJSON
	inspectErr    error
	startErr      error
	createErr     error
	stopErr       error
	removeErr     error

	// networkInspectErr controls NetworkInspect behavior.
	networkInspectErr error

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

func (f *fakeDocker) ContainerStop(_ context.Context, _ string, _ container.StopOptions) error {
	f.calls = append(f.calls, "Stop")
	return f.stopErr
}

func (f *fakeDocker) ContainerRemove(_ context.Context, _ string, _ container.RemoveOptions) error {
	f.calls = append(f.calls, "Remove")
	return f.removeErr
}

func (f *fakeDocker) NetworkInspect(_ context.Context, _ string, _ network.InspectOptions) (network.Inspect, error) {
	f.calls = append(f.calls, "NetworkInspect")
	return network.Inspect{}, f.networkInspectErr
}

func (f *fakeDocker) NetworkCreate(_ context.Context, _ string, _ network.CreateOptions) (network.CreateResponse, error) {
	f.calls = append(f.calls, "NetworkCreate")
	return network.CreateResponse{}, nil
}

func (f *fakeDocker) ImagePull(_ context.Context, _ string, _ image.PullOptions) (io.ReadCloser, error) {
	f.calls = append(f.calls, "Pull")
	return io.NopCloser(strings.NewReader("")), nil
}

func (f *fakeDocker) ContainerExecCreate(_ context.Context, _ string, _ container.ExecOptions) (types.IDResponse, error) {
	f.calls = append(f.calls, "Exec")
	return types.IDResponse{ID: "fake-exec-id"}, nil
}

func (f *fakeDocker) ContainerExecAttach(_ context.Context, _ string, _ container.ExecAttachOptions) (types.HijackedResponse, error) {
	return types.HijackedResponse{
		Reader: bufio.NewReader(bytes.NewReader(nil)),
		Conn:   &nopConn{},
	}, nil
}

// nopConn implements net.Conn for test use.
type nopConn struct{}

func (nopConn) Read([]byte) (int, error)         { return 0, io.EOF }
func (nopConn) Write(b []byte) (int, error)       { return len(b), nil }
func (nopConn) Close() error                      { return nil }
func (nopConn) LocalAddr() net.Addr               { return nil }
func (nopConn) RemoteAddr() net.Addr              { return nil }
func (nopConn) SetDeadline(time.Time) error       { return nil }
func (nopConn) SetReadDeadline(time.Time) error   { return nil }
func (nopConn) SetWriteDeadline(time.Time) error  { return nil }

func (f *fakeDocker) ContainerExecInspect(_ context.Context, _ string) (container.ExecInspect, error) {
	return container.ExecInspect{ExitCode: 0}, nil
}

func testKey(t *testing.T) wgtypes.Key {
	t.Helper()
	key, err := wgtypes.GeneratePrivateKey()
	if err != nil {
		t.Fatalf("generate key: %v", err)
	}
	return key
}

func testConfig(t *testing.T) Config {
	t.Helper()
	return Config{
		Interface:     "ployz0",
		MTU:           1420,
		PrivateKey:    testKey(t),
		Port:          51820,
		Image:         "test/wireguard",
		ContainerName: "test-wireguard",
		NetworkName:   "test-mesh",
	}
}

func newTestWG(t *testing.T, docker *fakeDocker) *WG {
	t.Helper()
	return New(testConfig(t), docker)
}

func TestUp_ReusesRunningContainer(t *testing.T) {
	docker := &fakeDocker{
		inspectResult: types.ContainerJSON{
			ContainerJSONBase: &types.ContainerJSONBase{
				State: &types.ContainerState{Running: true},
			},
		},
	}
	wg := newTestWG(t, docker)

	// Up will fail at configureInterface (exec not mocked) but
	// we verify ensureContainer reused the running container.
	_ = wg.Up(context.Background())

	if !contains(docker.calls, "Inspect") {
		t.Error("expected Inspect call")
	}
	if contains(docker.calls, "Create") {
		t.Error("should not create when container is running")
	}
}

func TestUp_StartsExistingStoppedContainer(t *testing.T) {
	docker := &fakeDocker{
		inspectResult: types.ContainerJSON{
			ContainerJSONBase: &types.ContainerJSONBase{
				State: &types.ContainerState{Running: false},
			},
		},
	}
	wg := newTestWG(t, docker)

	_ = wg.Up(context.Background())

	if !contains(docker.calls, "Start") {
		t.Error("expected Start call for stopped container")
	}
	if contains(docker.calls, "Create") {
		t.Error("should not create when container exists but stopped")
	}
}

func TestUp_CreatesWhenMissing(t *testing.T) {
	docker := &fakeDocker{
		inspectErr:        errdefs.ErrNotFound,
		networkInspectErr: errdefs.ErrNotFound,
	}
	wg := newTestWG(t, docker)

	_ = wg.Up(context.Background())

	want := []string{"NetworkInspect", "NetworkCreate", "Inspect", "Create", "Start"}
	for _, w := range want {
		if !contains(docker.calls, w) {
			t.Errorf("expected %s call, got %v", w, docker.calls)
		}
	}
}

func TestUp_WrapsInspectError(t *testing.T) {
	inspectErr := errors.New("docker daemon unreachable")
	docker := &fakeDocker{
		inspectErr: inspectErr,
	}
	wg := newTestWG(t, docker)

	err := wg.Up(context.Background())
	if err == nil {
		t.Fatal("Up should return an error")
	}
	if !errors.Is(err, inspectErr) {
		t.Errorf("got %v, want wrapped %v", err, inspectErr)
	}
}

func TestDown_StopsAndRemoves(t *testing.T) {
	docker := &fakeDocker{}
	wg := newTestWG(t, docker)

	if err := wg.Down(context.Background()); err != nil {
		t.Fatalf("Down: %v", err)
	}

	want := []string{"Stop", "Remove"}
	if !slices.Equal(docker.calls, want) {
		t.Errorf("calls = %v, want %v", docker.calls, want)
	}
}

func TestDown_IdempotentWhenNotFound(t *testing.T) {
	docker := &fakeDocker{
		stopErr:   errdefs.ErrNotFound,
		removeErr: errdefs.ErrNotFound,
	}
	wg := newTestWG(t, docker)

	if err := wg.Down(context.Background()); err != nil {
		t.Fatalf("Down should succeed when container not found: %v", err)
	}
}

func TestDown_WrapsStopError(t *testing.T) {
	stopErr := errors.New("cannot stop")
	docker := &fakeDocker{
		stopErr: stopErr,
	}
	wg := newTestWG(t, docker)

	err := wg.Down(context.Background())
	if err == nil {
		t.Fatal("Down should return an error")
	}
	if !errors.Is(err, stopErr) {
		t.Errorf("got %v, want wrapped %v", err, stopErr)
	}
}

func contains(haystack []string, needle string) bool {
	for _, s := range haystack {
		if s == needle {
			return true
		}
	}
	return false
}

