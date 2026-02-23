package fake

import (
	"context"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/mesh"
)

var _ mesh.PlatformOps = (*PlatformOps)(nil)

const (
	FaultPlatformPrepare            = "platform_ops.prepare"
	FaultPlatformConfigureWireGuard = "platform_ops.configure_wireguard"
	FaultPlatformEnsureDocker       = "platform_ops.ensure_docker_network"
	FaultPlatformCleanupDocker      = "platform_ops.cleanup_docker_network"
	FaultPlatformCleanupWireGuard   = "platform_ops.cleanup_wireguard"
	FaultPlatformAfterStart         = "platform_ops.after_start"
	FaultPlatformAfterStop          = "platform_ops.after_stop"
	FaultPlatformApplyPeerConfig    = "platform_ops.apply_peer_config"
)

// PlatformOps is a no-op implementation of mesh.PlatformOps that records calls.
type PlatformOps struct {
	CallRecorder
	mu     sync.Mutex
	Peers  []mesh.Peer
	faults *fault.Injector

	PrepareErr              func(ctx context.Context, cfg mesh.Config, store mesh.StateStore) error
	ConfigureWireGuardErr   func(ctx context.Context, cfg mesh.Config, state *mesh.State) error
	EnsureDockerNetworkErr  func(ctx context.Context, cfg mesh.Config, state *mesh.State) error
	CleanupDockerNetworkErr func(ctx context.Context, cfg mesh.Config, state *mesh.State) error
	CleanupWireGuardErr     func(ctx context.Context, cfg mesh.Config, state *mesh.State) error
	AfterStartErr           func(ctx context.Context, cfg mesh.Config) error
	AfterStopErr            func(ctx context.Context, cfg mesh.Config, state *mesh.State) error
	ApplyPeerConfigErr      func(ctx context.Context, cfg mesh.Config, state *mesh.State, peers []mesh.Peer) error
}

func (o *PlatformOps) FailOnce(point string, err error) {
	o.ensureFaults().FailOnce(point, err)
}

func (o *PlatformOps) FailAlways(point string, err error) {
	o.ensureFaults().FailAlways(point, err)
}

func (o *PlatformOps) SetFaultHook(point string, hook fault.Hook) {
	o.ensureFaults().SetHook(point, hook)
}

func (o *PlatformOps) ClearFault(point string) {
	o.ensureFaults().Clear(point)
}

func (o *PlatformOps) ResetFaults() {
	o.ensureFaults().Reset()
}

func (o *PlatformOps) evalFault(point string, args ...any) error {
	check.Assert(o != nil, "PlatformOps.evalFault: receiver must not be nil")
	if o == nil {
		return nil
	}
	return o.ensureFaults().Eval(point, args...)
}

func (o *PlatformOps) ensureFaults() *fault.Injector {
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.faults == nil {
		o.faults = fault.NewInjector()
	}
	return o.faults
}

func (o *PlatformOps) Prepare(ctx context.Context, cfg mesh.Config, store mesh.StateStore) error {
	o.record("Prepare", cfg, store)
	if err := o.evalFault(FaultPlatformPrepare, ctx, cfg, store); err != nil {
		return err
	}
	if o.PrepareErr != nil {
		return o.PrepareErr(ctx, cfg, store)
	}
	return nil
}

func (o *PlatformOps) ConfigureWireGuard(ctx context.Context, cfg mesh.Config, state *mesh.State) error {
	o.record("ConfigureWireGuard", cfg, state)
	if err := o.evalFault(FaultPlatformConfigureWireGuard, ctx, cfg, state); err != nil {
		return err
	}
	if o.ConfigureWireGuardErr != nil {
		return o.ConfigureWireGuardErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) EnsureDockerNetwork(ctx context.Context, cfg mesh.Config, state *mesh.State) error {
	o.record("EnsureDockerNetwork", cfg, state)
	if err := o.evalFault(FaultPlatformEnsureDocker, ctx, cfg, state); err != nil {
		return err
	}
	if o.EnsureDockerNetworkErr != nil {
		return o.EnsureDockerNetworkErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) CleanupDockerNetwork(ctx context.Context, cfg mesh.Config, state *mesh.State) error {
	o.record("CleanupDockerNetwork", cfg, state)
	if err := o.evalFault(FaultPlatformCleanupDocker, ctx, cfg, state); err != nil {
		return err
	}
	if o.CleanupDockerNetworkErr != nil {
		return o.CleanupDockerNetworkErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) CleanupWireGuard(ctx context.Context, cfg mesh.Config, state *mesh.State) error {
	o.record("CleanupWireGuard", cfg, state)
	if err := o.evalFault(FaultPlatformCleanupWireGuard, ctx, cfg, state); err != nil {
		return err
	}
	if o.CleanupWireGuardErr != nil {
		return o.CleanupWireGuardErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) AfterStart(ctx context.Context, cfg mesh.Config) error {
	o.record("AfterStart", cfg)
	if err := o.evalFault(FaultPlatformAfterStart, ctx, cfg); err != nil {
		return err
	}
	if o.AfterStartErr != nil {
		return o.AfterStartErr(ctx, cfg)
	}
	return nil
}

func (o *PlatformOps) AfterStop(ctx context.Context, cfg mesh.Config, state *mesh.State) error {
	o.record("AfterStop", cfg, state)
	if err := o.evalFault(FaultPlatformAfterStop, ctx, cfg, state); err != nil {
		return err
	}
	if o.AfterStopErr != nil {
		return o.AfterStopErr(ctx, cfg, state)
	}
	return nil
}

func (o *PlatformOps) ApplyPeerConfig(ctx context.Context, cfg mesh.Config, state *mesh.State, peers []mesh.Peer) error {
	o.record("ApplyPeerConfig", cfg, state, peers)
	if err := o.evalFault(FaultPlatformApplyPeerConfig, ctx, cfg, state, peers); err != nil {
		return err
	}
	if o.ApplyPeerConfigErr != nil {
		return o.ApplyPeerConfigErr(ctx, cfg, state, peers)
	}
	o.mu.Lock()
	defer o.mu.Unlock()

	o.Peers = make([]mesh.Peer, len(peers))
	copy(o.Peers, peers)
	return nil
}

// HasPeer reports whether a peer with the given public key has been applied.
func (o *PlatformOps) HasPeer(publicKey string) bool {
	o.mu.Lock()
	defer o.mu.Unlock()
	for _, p := range o.Peers {
		if p.PublicKey == publicKey {
			return true
		}
	}
	return false
}
