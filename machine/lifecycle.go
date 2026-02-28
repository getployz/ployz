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
	m.shutdown(context.Background())
	return nil
}

func (m *Machine) shutdown(ctx context.Context) {
	if m.mesh == nil {
		return
	}
	if err := m.mesh.Stop(ctx); err != nil {
		slog.Error("mesh shutdown", "err", err)
	}
}

