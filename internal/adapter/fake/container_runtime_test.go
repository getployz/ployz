package fake

import (
	"context"
	"errors"
	"net/netip"
	"testing"

	"ployz/internal/mesh"
)

func TestContainerRuntime_Lifecycle(t *testing.T) {
	ctx := t.Context()
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
	ctx := t.Context()
	rt := NewContainerRuntime()

	_ = rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{Name: "c1"})
	_ = rt.ContainerStart(ctx, "c1")
	if err := rt.ContainerRemove(ctx, "c1", true); err != nil {
		t.Fatalf("force remove should succeed on running container: %v", err)
	}
}

func TestContainerRuntime_Network(t *testing.T) {
	ctx := t.Context()
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
	ctx := t.Context()
	rt := NewContainerRuntime()
	rt.SetReady(false)

	if err := rt.WaitReady(ctx); err == nil {
		t.Error("expected error when not ready")
	}
}

func TestContainerRuntime_ErrorInjection(t *testing.T) {
	ctx := t.Context()
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

func TestContainerRuntime_FaultFailOnce(t *testing.T) {
	ctx := t.Context()
	rt := NewContainerRuntime()
	injected := errors.New("injected once")
	rt.FailOnce(FaultContainerRuntimeCreate, injected)

	err := rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{Name: "c1"})
	if !errors.Is(err, injected) {
		t.Fatalf("first ContainerCreate error = %v, want %v", err, injected)
	}

	err = rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{Name: "c1"})
	if err != nil {
		t.Fatalf("second ContainerCreate error = %v, want nil", err)
	}
}

func TestContainerRuntime_FaultHook(t *testing.T) {
	ctx := t.Context()
	rt := NewContainerRuntime()
	injected := errors.New("blocked image")
	rt.SetFaultHook(FaultContainerRuntimeImagePull, func(args ...any) error {
		if len(args) < 2 {
			return nil
		}
		image, _ := args[1].(string)
		if image == "bad:latest" {
			return injected
		}
		return nil
	})

	err := rt.ImagePull(ctx, "bad:latest")
	if !errors.Is(err, injected) {
		t.Fatalf("bad image pull error = %v, want %v", err, injected)
	}

	err = rt.ImagePull(ctx, "alpine:latest")
	if err != nil {
		t.Fatalf("good image pull error = %v, want nil", err)
	}
}

func TestContainerRuntime_ImagePull(t *testing.T) {
	ctx := t.Context()
	rt := NewContainerRuntime()

	if err := rt.ImagePull(ctx, "alpine:latest"); err != nil {
		t.Fatal(err)
	}
	if len(rt.Calls("ImagePull")) != 1 {
		t.Error("expected 1 ImagePull call")
	}
}

func TestContainerRuntime_StartMissing(t *testing.T) {
	ctx := t.Context()
	rt := NewContainerRuntime()

	if err := rt.ContainerStart(ctx, "nonexistent"); err == nil {
		t.Error("expected error starting nonexistent container")
	}
}

func TestContainerRuntime_ContainerListLabelFilter(t *testing.T) {
	ctx := t.Context()
	rt := NewContainerRuntime()

	_ = rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{
		Name:   "api-1",
		Image:  "api:latest",
		Labels: map[string]string{"ployz.namespace": "frontend", "service": "api"},
	})
	_ = rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{
		Name:   "db-1",
		Image:  "postgres:16",
		Labels: map[string]string{"ployz.namespace": "backend", "service": "db"},
	})
	_ = rt.ContainerStart(ctx, "api-1")

	entries, err := rt.ContainerList(ctx, map[string]string{"ployz.namespace": "frontend"})
	if err != nil {
		t.Fatalf("ContainerList() error = %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("ContainerList() len = %d, want 1", len(entries))
	}
	if entries[0].Name != "api-1" {
		t.Fatalf("ContainerList()[0].Name = %q, want %q", entries[0].Name, "api-1")
	}
	if !entries[0].Running {
		t.Fatal("ContainerList()[0].Running = false, want true")
	}
}

func TestContainerRuntime_ContainerUpdate(t *testing.T) {
	ctx := t.Context()
	rt := NewContainerRuntime()

	_ = rt.ContainerCreate(ctx, mesh.ContainerCreateConfig{Name: "api-1", Image: "api:latest"})
	err := rt.ContainerUpdate(ctx, "api-1", mesh.ResourceConfig{CPULimit: 2, MemoryLimit: 256 * 1024 * 1024})
	if err != nil {
		t.Fatalf("ContainerUpdate() error = %v", err)
	}

	if err := rt.ContainerUpdate(ctx, "missing", mesh.ResourceConfig{CPULimit: 1}); err == nil {
		t.Fatal("ContainerUpdate() expected missing container error")
	}
}
