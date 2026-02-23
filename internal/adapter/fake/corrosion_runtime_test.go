package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/mesh"
)

func TestCorrosionRuntime_Lifecycle(t *testing.T) {
	ctx := t.Context()
	cr := NewCorrosionRuntime()

	cfg := mesh.CorrosionConfig{Name: "corrosion-testnet", Image: "corrosion:latest"}
	if err := cr.WriteConfig(cfg); err != nil {
		t.Fatal(err)
	}
	if cr.LastConfig.Name != "corrosion-testnet" {
		t.Errorf("expected last config name 'corrosion-testnet', got %q", cr.LastConfig.Name)
	}

	if err := cr.Start(ctx, cfg); err != nil {
		t.Fatal(err)
	}
	if !cr.Running["corrosion-testnet"] {
		t.Error("expected corrosion-testnet to be running")
	}

	if err := cr.Stop(ctx, "corrosion-testnet"); err != nil {
		t.Fatal(err)
	}
	if cr.Running["corrosion-testnet"] {
		t.Error("expected corrosion-testnet to be stopped")
	}
}

func TestCorrosionRuntime_ErrorInjection(t *testing.T) {
	ctx := t.Context()
	cr := NewCorrosionRuntime()
	injected := errors.New("permission denied")

	cr.StartErr = func(_ context.Context, cfg mesh.CorrosionConfig) error {
		if cfg.Name == "corrosion-testnet" {
			return injected
		}
		return nil
	}

	cfg := mesh.CorrosionConfig{Name: "corrosion-testnet"}
	if err := cr.Start(ctx, cfg); !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}
	if cr.Running["corrosion-testnet"] {
		t.Error("should not be running after start error")
	}
}

func TestCorrosionRuntime_CallRecording(t *testing.T) {
	ctx := t.Context()
	cr := NewCorrosionRuntime()

	cfg := mesh.CorrosionConfig{Name: "c1"}
	_ = cr.WriteConfig(cfg)
	_ = cr.Start(ctx, cfg)
	_ = cr.Stop(ctx, "c1")

	if len(cr.Calls("WriteConfig")) != 1 {
		t.Errorf("expected 1 WriteConfig call, got %d", len(cr.Calls("WriteConfig")))
	}
	if len(cr.Calls("Start")) != 1 {
		t.Errorf("expected 1 Start call, got %d", len(cr.Calls("Start")))
	}
	if len(cr.Calls("Stop")) != 1 {
		t.Errorf("expected 1 Stop call, got %d", len(cr.Calls("Stop")))
	}
}

func TestCorrosionRuntime_FaultFailOnce(t *testing.T) {
	ctx := t.Context()
	cr := NewCorrosionRuntime()
	injected := errors.New("injected")
	cr.FailOnce(FaultCorrosionStart, injected)

	cfg := mesh.CorrosionConfig{Name: "c1"}
	err := cr.Start(ctx, cfg)
	if !errors.Is(err, injected) {
		t.Fatalf("first Start() error = %v, want injected", err)
	}

	err = cr.Start(ctx, cfg)
	if err != nil {
		t.Fatalf("second Start() error = %v, want nil", err)
	}
	if !cr.Running["c1"] {
		t.Fatal("second Start() should mark c1 running")
	}
}
