package fake

import (
	"context"
	"errors"
	"testing"

	"ployz/internal/network"
)

func TestPlatformOps_AllMethodsRecordCalls(t *testing.T) {
	ctx := context.Background()
	ops := &PlatformOps{}
	cfg := network.Config{}
	state := &network.State{}
	peers := []network.Peer{{PublicKey: "key1"}}

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
	ctx := context.Background()
	ops := &PlatformOps{}
	peers := []network.Peer{
		{PublicKey: "key1", Endpoint: "1.2.3.4:51820"},
		{PublicKey: "key2", Endpoint: "5.6.7.8:51820"},
	}

	if err := ops.ApplyPeerConfig(ctx, network.Config{}, &network.State{}, peers); err != nil {
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
	ctx := context.Background()
	ops := &PlatformOps{}
	injected := errors.New("wg setup failed")

	ops.ConfigureWireGuardErr = func(context.Context, network.Config, *network.State) error {
		return injected
	}

	if err := ops.ConfigureWireGuard(ctx, network.Config{}, &network.State{}); !errors.Is(err, injected) {
		t.Errorf("expected injected error, got %v", err)
	}

	// Other methods should still succeed.
	if err := ops.Prepare(ctx, network.Config{}, nil); err != nil {
		t.Errorf("expected nil error from Prepare, got %v", err)
	}
}
