//go:build !linux && !darwin

package platform

import (
	"context"
	"errors"

	"ployz/internal/adapter/sqlite"
	"ployz/internal/network"
)

// NewController creates a Controller with stub implementations for unsupported platforms.
func NewController(opts ...network.Option) (*network.Controller, error) {
	defaults := []network.Option{
		network.WithStatusProber(stubStatusProber{}),
		network.WithStateStore(sqlite.NetworkStateStore{}),
		network.WithClock(network.RealClock{}),
		network.WithPlatformOps(stubPlatformOps{}),
	}
	return network.New(append(defaults, opts...)...)
}

type stubPlatformOps struct{}

func (stubPlatformOps) Prepare(_ context.Context, _ network.Config, _ network.StateStore) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) ConfigureWireGuard(_ context.Context, _ network.Config, _ *network.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) EnsureDockerNetwork(_ context.Context, _ network.Config, _ *network.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) CleanupDockerNetwork(_ context.Context, _ network.Config, _ *network.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) CleanupWireGuard(_ context.Context, _ network.Config, _ *network.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) AfterStart(_ context.Context, _ network.Config) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) AfterStop(_ context.Context, _ network.Config, _ *network.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) ApplyPeerConfig(_ context.Context, _ network.Config, _ *network.State, _ []network.Peer) error {
	return errors.New("platform not supported")
}

type stubStatusProber struct{}

func (stubStatusProber) ProbeInfra(_ context.Context, _ *network.State) (bool, bool, bool, error) {
	return false, false, false, nil
}
