package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/mesh"
)

func TestPlatformOps_AllMethodsRecordCalls(t *testing.T) {
	ctx := t.Context()
	ops := &PlatformOps{}
	cfg := mesh.Config{}
	state := &mesh.State{}
	peers := []mesh.Peer{{PublicKey: "key1"}}

	_ = ops.Prepare(ctx, cfg, nil)
	_ = ops.ConfigureWireGuard(ctx, cfg, state)
	_ = ops.EnsureDockerNetwork(ctx, cfg, state)
	_ = ops.CleanupDockerNetwork(ctx, cfg, state)
	_ = ops.CleanupWireGuard(ctx, cfg, state)
	_ = ops.AfterStart(ctx, cfg)
	_ = ops.AfterStop(ctx, cfg, state)
	_ = ops.ApplyPeerConfig(ctx, cfg, state, peers)

	all := ops.Calls("")
	if len(all) != 8 {
		t.Fatalf("expected 8 calls, got %d", len(all))
	}

	methods := []string{
		"Prepare", "ConfigureWireGuard", "EnsureDockerNetwork",
		"CleanupDockerNetwork", "CleanupWireGuard", "AfterStart",
		"AfterStop", "ApplyPeerConfig",
	}
	for _, m := range methods {
		if len(ops.Calls(m)) != 1 {
			t.Errorf("expected 1 %s call, got %d", m, len(ops.Calls(m)))
		}
	}
}

func TestPlatformOps_ApplyPeerConfigCaptures(t *testing.T) {
	ctx := t.Context()
	ops := &PlatformOps{}
	peers := []mesh.Peer{
		{PublicKey: "key1", Endpoint: "1.2.3.4:51820"},
		{PublicKey: "key2", Endpoint: "5.6.7.8:51820"},
	}

	if err := ops.ApplyPeerConfig(ctx, mesh.Config{}, &mesh.State{}, peers); err != nil {
		t.Fatal(err)
	}
	if len(ops.Peers) != 2 {
		t.Fatalf("expected 2 captured peers, got %d", len(ops.Peers))
	}
	if ops.Peers[0].PublicKey != "key1" {
		t.Errorf("expected first peer key 'key1', got %q", ops.Peers[0].PublicKey)
	}
}

func TestPlatformOps_ErrorInjection(t *testing.T) {
	ctx := t.Context()
	ops := &PlatformOps{}
	injected := errors.New("wg setup failed")

	ops.ConfigureWireGuardErr = func(context.Context, mesh.Config, *mesh.State) error {
		return injected
	}

	if err := ops.ConfigureWireGuard(ctx, mesh.Config{}, &mesh.State{}); !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}

	// Other methods should still succeed.
	if err := ops.Prepare(ctx, mesh.Config{}, nil); err != nil {
		t.Errorf("expected nil error from Prepare, got %v", err)
	}
}

func TestPlatformOps_FaultFailOnce(t *testing.T) {
	ctx := t.Context()
	ops := &PlatformOps{}
	injected := errors.New("injected")
	ops.FailOnce(FaultPlatformPrepare, injected)

	err := ops.Prepare(ctx, mesh.Config{}, nil)
	if !errors.Is(err, injected) {
		t.Fatalf("first Prepare() error = %v, want injected", err)
	}

	err = ops.Prepare(ctx, mesh.Config{}, nil)
	if err != nil {
		t.Fatalf("second Prepare() error = %v, want nil", err)
	}
}

func TestPlatformOps_FaultPoints(t *testing.T) {
	ctx := t.Context()
	cfg := mesh.Config{}
	state := &mesh.State{}
	peers := []mesh.Peer{{PublicKey: "key1"}}

	tests := []struct {
		name  string
		point string
		run   func(*PlatformOps) error
	}{
		{
			name:  "prepare",
			point: FaultPlatformPrepare,
			run: func(ops *PlatformOps) error {
				return ops.Prepare(ctx, cfg, nil)
			},
		},
		{
			name:  "configure wireguard",
			point: FaultPlatformConfigureWireGuard,
			run: func(ops *PlatformOps) error {
				return ops.ConfigureWireGuard(ctx, cfg, state)
			},
		},
		{
			name:  "ensure docker network",
			point: FaultPlatformEnsureDocker,
			run: func(ops *PlatformOps) error {
				return ops.EnsureDockerNetwork(ctx, cfg, state)
			},
		},
		{
			name:  "cleanup docker network",
			point: FaultPlatformCleanupDocker,
			run: func(ops *PlatformOps) error {
				return ops.CleanupDockerNetwork(ctx, cfg, state)
			},
		},
		{
			name:  "cleanup wireguard",
			point: FaultPlatformCleanupWireGuard,
			run: func(ops *PlatformOps) error {
				return ops.CleanupWireGuard(ctx, cfg, state)
			},
		},
		{
			name:  "after start",
			point: FaultPlatformAfterStart,
			run: func(ops *PlatformOps) error {
				return ops.AfterStart(ctx, cfg)
			},
		},
		{
			name:  "after stop",
			point: FaultPlatformAfterStop,
			run: func(ops *PlatformOps) error {
				return ops.AfterStop(ctx, cfg, state)
			},
		},
		{
			name:  "apply peer config",
			point: FaultPlatformApplyPeerConfig,
			run: func(ops *PlatformOps) error {
				return ops.ApplyPeerConfig(ctx, cfg, state, peers)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ops := &PlatformOps{}
			injected := errors.New("injected")
			ops.FailOnce(tt.point, injected)

			err := tt.run(ops)
			if !errors.Is(err, injected) {
				t.Fatalf("first call error = %v, want injected", err)
			}

			err = tt.run(ops)
			if err != nil {
				t.Fatalf("second call error = %v, want nil", err)
			}
		})
	}
}
