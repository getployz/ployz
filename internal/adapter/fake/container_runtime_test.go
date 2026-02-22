package fake

import (
	"context"
	"errors"
	"net/netip"
	"testing"

	"ployz/internal/mesh"
)

func TestContainerRuntime_Lifecycle(t *testing.T) {
	ctx := context.Background()
	rt := NewContainerRuntime()

	// Create container
	cfg := mesh.ContainerCreateConfig{Name: "test-container", Image: "alpine:latest"}
	if err := rt.ContainerCreate(ctx, cfg); err != nil {
		t.Fatal(err)
	}

	// Inspect — exists but not running
	info, err := rt.ContainerInspect(ctx, "test-container")
	if err != nil {
		t.Fatal(err)
	}
	if !info.Exists || info.Running {
		t.Errorf("expected exists=true running=false, got exists=%v running=%v", info.Exists, info.Running)
	}

	// Start
	if err := rt.ContainerStart(ctx, "test-container"); err != nil {
		t.Fatal(err)
	}
	info, _ = rt.ContainerInspect(ctx, "test-container")
	if !info.Running {
		t.Error("expected running after start")
	}

	// Remove without force on running container should fail
	if err := rt.ContainerRemove(ctx, "test-container", false); err == nil {
		t.Error("expected error removing running container without force")
	}

	// Stop
	if err := rt.ContainerStop(ctx, "test-container"); err != nil {
		t.Fatal(err)
	}
	info, _ = rt.ContainerInspect(ctx, "test-container")
	if info.Running {
		t.Error("expected not running after stop")
	}

	// Remove
	if err := rt.ContainerRemove(ctx, "test-container", false); err != nil {
		t.Fatal(err)
	}
	info, _ = rt.ContainerInspect(ctx, "test-container")
	if info.Exists {
		t.Error("expected container to not exist after remove")
	}
}

func TestContainerRuntime_ForceRemoveRunning(t *testing.T) {
	ctx := context.Background()
	rt := NewContainerRuntime()

	_ = rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{Name: "c1"})
	_ = rt.ContainerStart(ctx, "c1")
	if err := rt.ContainerRemove(ctx, "c1", true); err != nil {
		t.Fatalf("force remove should succeed on running container: %v", err)
	}
}

func TestContainerRuntime_Network(t *testing.T) {
	ctx := context.Background()
	rt := NewContainerRuntime()

	subnet := netip.MustParsePrefix("10.0.0.0/24")
	if err := rt.NetworkCreate(ctx, "test-net", subnet, "wg0"); err != nil {
		t.Fatal(err)
	}

	info, err := rt.NetworkInspect(ctx, "test-net")
	if err != nil {
		t.Fatal(err)
	}
	if !info.Exists || info.Subnet != "10.0.0.0/24" {
		t.Errorf("unexpected network info: %+v", info)
	}

	if err := rt.NetworkRemove(ctx, "test-net"); err != nil {
		t.Fatal(err)
	}
	info, _ = rt.NetworkInspect(ctx, "test-net")
	if info.Exists {
		t.Error("expected network to not exist after remove")
	}
}

func TestContainerRuntime_NotReady(t *testing.T) {
	ctx := context.Background()
	rt := NewContainerRuntime()
	rt.SetReady(false)

	if err := rt.WaitReady(ctx); err == nil {
		t.Error("expected error when not ready")
	}
}

func TestContainerRuntime_ErrorInjection(t *testing.T) {
	ctx := context.Background()
	rt := NewContainerRuntime()
	injected := errors.New("disk full")

	rt.ContainerCreateErr = func(_ context.Context, cfg mesh.ContainerCreateConfig) error {
		if cfg.Name == "bad" {
			return injected
		}
		return nil
	}

	if err := rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{Name: "bad"}); !errors.Is(err, injected) {
		t.Errorf("expected injected error for 'bad', got %v", err)
	}
	if err := rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{Name: "good"}); err != nil {
		t.Errorf("expected no error for 'good', got %v", err)
	}
}

func TestContainerRuntime_ImagePull(t *testing.T) {
	ctx := context.Background()
	rt := NewContainerRuntime()

	if err := rt.ImagePull(ctx, "alpine:latest"); err != nil {
		t.Fatal(err)
	}
	if len(rt.Calls("ImagePull")) != 1 {
		t.Error("expected 1 ImagePull call")
	}
}

func TestContainerRuntime_StartMissing(t *testing.T) {
	ctx := context.Background()
	rt := NewContainerRuntime()

	if err := rt.ContainerStart(ctx, "nonexistent"); err == nil {
		t.Error("expected error starting nonexistent container")
	}
}
