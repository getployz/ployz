package machine

import (
	"context"
	"fmt"
	"log/slog"
)

// Run starts the machine. If a network config exists from a previous init/join,
// the mesh is started automatically. Then blocks until ctx is cancelled.
func (m *Machine) Run(ctx context.Context) error {
	if m.mesh != nil && m.hasNetworkConfig() {
		if err := m.mesh.Start(ctx); err != nil {
			return fmt.Errorf("start mesh: %w", err)
		}
		slog.Info("Mesh started from existing config.")
	}

	close(m.started)

	<-ctx.Done()
	return m.shutdown(context.Background())
}

func (m *Machine) shutdown(ctx context.Context) error {
	if m.mesh == nil {
		return nil
	}
	if err := m.mesh.Stop(ctx); err != nil {
		slog.Error("mesh shutdown", "err", err)
	}
	return nil
}

// EnableNetwork starts the mesh. Returns an error if no mesh is configured.
func (m *Machine) EnableNetwork(ctx context.Context) error {
	if m.mesh == nil {
		return fmt.Errorf("no mesh configured")
	}
	return m.mesh.Start(ctx)
}

// DisableNetwork stops the mesh.
func (m *Machine) DisableNetwork(ctx context.Context) error {
	if m.mesh == nil {
		return nil
	}
	return m.mesh.Stop(ctx)
}
