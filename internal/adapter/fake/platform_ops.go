package fake

import (
	"context"
	"sync"

	"ployz/internal/network"
)

var _ network.PlatformOps = (*PlatformOps)(nil)

// PlatformOps is a no-op implementation of network.PlatformOps that records calls.
type PlatformOps struct {
	CallRecorder
	mu    sync.Mutex
	Peers []network.Peer

	PrepareErr            func(ctx context.Context, cfg network.Config, store network.StateStore) error
	ConfigureWireGuardErr func(ctx context.Context, cfg network.Config, state *network.State) error
	EnsureDockerNetworkErr func(ctx context.Context, cfg network.Config, state *network.State) error
	CleanupDockerNetworkErr func(ctx context.Context, cfg network.Config, state *network.State) error
	CleanupWireGuardErr   func(ctx context.Context, cfg network.Config, state *network.State) error
	AfterStartErr         func(ctx context.Context, cfg network.Config) error
	AfterStopErr          func(ctx context.Context, cfg network.Config, state *network.State) error
	ApplyPeerConfigErr    func(ctx context.Context, cfg network.Config, state *network.State, peers []network.Peer) error
}

func (o *PlatformOps) Prepare(ctx context.Context, cfg network.Config, store network.StateStore) error {
	o.record("Prepare", cfg, store)
	if o.PrepareErr != nil {
		return o.PrepareErr(ctx, cfg, store)
	}
	return nil
}

func (o *PlatformOps) ConfigureWireGuard(ctx context.Context, cfg network.Config, state *network.State) error {
	o.record("ConfigureWireGuard", cfg, state)
	if o.ConfigureWireGuardErr != nil {
		return o.ConfigureWireGuardErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) EnsureDockerNetwork(ctx context.Context, cfg network.Config, state *network.State) error {
	o.record("EnsureDockerNetwork", cfg, state)
	if o.EnsureDockerNetworkErr != nil {
		return o.EnsureDockerNetworkErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) CleanupDockerNetwork(ctx context.Context, cfg network.Config, state *network.State) error {
	o.record("CleanupDockerNetwork", cfg, state)
	if o.CleanupDockerNetworkErr != nil {
		return o.CleanupDockerNetworkErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) CleanupWireGuard(ctx context.Context, cfg network.Config, state *network.State) error {
	o.record("CleanupWireGuard", cfg, state)
	if o.CleanupWireGuardErr != nil {
		return o.CleanupWireGuardErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) AfterStart(ctx context.Context, cfg network.Config) error {
	o.record("AfterStart", cfg)
	if o.AfterStartErr != nil {
		return o.AfterStartErr(ctx, cfg)
	}
	return nil
}

func (o *PlatformOps) AfterStop(ctx context.Context, cfg network.Config, state *network.State) error {
	o.record("AfterStop", cfg, state)
	if o.AfterStopErr != nil {
		return o.AfterStopErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) ApplyPeerConfig(ctx context.Context, cfg network.Config, state *network.State, peers []network.Peer) error {
	o.record("ApplyPeerConfig", cfg, state, peers)
	if o.ApplyPeerConfigErr != nil {
		return o.ApplyPeerConfigErr(ctx, cfg, state, peers)
	}
	o.mu.Lock()
	defer o.mu.Unlock()

	o.Peers = make([]network.Peer, len(peers))
	copy(o.Peers, peers)
	return nil
}
