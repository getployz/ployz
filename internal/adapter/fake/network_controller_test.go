package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/mesh"
)

func TestNetworkController_StartClose(t *testing.T) {
	ctx := context.Background()
	returnCfg := mesh.Config{Network: "test-net"}
	ctrl := NewNetworkController(returnCfg)

	cfg, err := ctrl.Start(ctx, mesh.Config{})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Network != "test-net" {
		t.Errorf("expected return config network 'test-net', got %q", cfg.Network)
	}
	if !ctrl.Started {
		t.Error("expected Started to be true")
	}

	if err := ctrl.Close(); err != nil {
		t.Fatal(err)
	}
	if !ctrl.Closed {
		t.Error("expected Closed to be true")
	}
}

func TestNetworkController_ErrorInjection(t *testing.T) {
	ctx := context.Background()
	injected := errors.New("start failed")
	ctrl := NewNetworkController(mesh.Config{})

	ctrl.StartErr = func(context.Context, mesh.Config) error { return injected }
	_, err := ctrl.Start(ctx, mesh.Config{})
	if !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}
	if ctrl.Started {
		t.Error("expected Started to be false after error")
	}
}

func TestNetworkController_Factory(t *testing.T) {
	ctrl := NewNetworkController(mesh.Config{Network: "net1"})
	factory := ControllerFactory(ctrl)

	nc, err := factory()
	if err != nil {
		t.Fatal(err)
	}
	if nc != ctrl {
		t.Error("expected factory to return same controller instance")
	}
}

func TestReconcilerFactory(t *testing.T) {
	rec := NewPeerReconciler()
	factory := ReconcilerFactory(rec)

	pr, err := factory()
	if err != nil {
		t.Fatal(err)
	}
	if pr != rec {
		t.Error("expected factory to return same reconciler instance")
	}
}

func TestNetworkController_CallRecording(t *testing.T) {
	ctx := context.Background()
	ctrl := NewNetworkController(mesh.Config{})

	_, _ = ctrl.Start(ctx, mesh.Config{})
	_ = ctrl.Close()

	if len(ctrl.Calls("Start")) != 1 {
		t.Error("expected 1 Start call")
	}
	if len(ctrl.Calls("Close")) != 1 {
		t.Error("expected 1 Close call")
	}
}
