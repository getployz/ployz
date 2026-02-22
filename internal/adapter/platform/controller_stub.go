//go:build !linux && !darwin

package platform

import (
	"context"
	"errors"

	"ployz/internal/adapter/sqlite"
	"ployz/internal/mesh"
)

// NewController creates a Controller with stub implementations for unsupported platforms.
func NewController(opts ...mesh.Option) (*mesh.Controller, error) {
	defaults := []mesh.Option{
		mesh.WithStatusProber(stubStatusProber{}),
		mesh.WithStateStore(sqlite.NetworkStateStore{}),
		mesh.WithClock(mesh.RealClock{}),
		mesh.WithPlatformOps(stubPlatformOps{}),
	}
	return mesh.New(append(defaults, opts...)...)
}

type stubPlatformOps struct{}

func (stubPlatformOps) Prepare(_ context.Context, _ mesh.Config, _ mesh.StateStore) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) ConfigureWireGuard(_ context.Context, _ mesh.Config, _ *mesh.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) EnsureDockerNetwork(_ context.Context, _ mesh.Config, _ *mesh.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) CleanupDockerNetwork(_ context.Context, _ mesh.Config, _ *mesh.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) CleanupWireGuard(_ context.Context, _ mesh.Config, _ *mesh.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) AfterStart(_ context.Context, _ mesh.Config) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) AfterStop(_ context.Context, _ mesh.Config, _ *mesh.State) error {
	return errors.New("platform not supported")
}
func (stubPlatformOps) ApplyPeerConfig(_ context.Context, _ mesh.Config, _ *mesh.State, _ []mesh.Peer) error {
	return errors.New("platform not supported")
}

type stubStatusProber struct{}

func (stubStatusProber) ProbeInfra(_ context.Context, _ *mesh.State) (bool, bool, bool, error) {
	return false, false, false, nil
}
