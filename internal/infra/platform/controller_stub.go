//go:build !linux && !darwin

package platform

import (
	"context"
	"errors"

	"ployz/internal/infra/sqlite"
	"ployz/internal/daemon/overlay"
)

// NewController creates an overlay service with stubs for unsupported platforms.
func NewController(opts ...overlay.Option) (*overlay.Service, error) {
	defaults := []overlay.Option{
		overlay.WithStatusProber(stubStatusProber{}),
		overlay.WithStateStore(sqlite.NetworkStateStore{}),
		overlay.WithClock(overlay.RealClock{}),
		overlay.WithPlatformOps(stubPlatformOps{}),
	}
	return overlay.NewService(append(defaults, opts...)...)
}

type stubPlatformOps struct{}

func (stubPlatformOps) Prepare(_ context.Context, _ overlay.Config, _ overlay.StateStore) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) ConfigureWireGuard(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) EnsureDockerNetwork(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) CleanupDockerNetwork(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) CleanupWireGuard(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) AfterStart(_ context.Context, _ overlay.Config) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) AfterStop(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) ApplyPeerConfig(_ context.Context, _ overlay.Config, _ *overlay.State, _ []overlay.Peer) error {
	return errors.New("platform not supported")
}

type stubStatusProber struct{}

func (stubStatusProber) ProbeInfra(_ context.Context, _ *overlay.State) (bool, bool, bool, error) {
	return false, false, false, nil
}
