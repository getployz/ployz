package machine

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"
)

const startupCleanupTimeout = 15 * time.Second

// Run starts the machine. If a network config exists from a previous init/join,
// the mesh is started automatically. Then blocks until ctx is cancelled.
func (m *Machine) Run(ctx context.Context) error {
	if m.mesh != nil && m.hasNetworkConfig() {
		if err := m.startMesh(ctx); err != nil {
			return fmt.Errorf("start mesh: %w", err)
		}
		slog.Info("Mesh started from existing config.")
	}

	close(m.started)

	<-ctx.Done()
	m.shutdown(context.Background())
	return nil
}

// startMesh brings up the mesh and destroys partial state on failure.
// Context cancellation is treated as intentional and skips cleanup.
func (m *Machine) startMesh(ctx context.Context) error {
	err := m.mesh.Up(ctx)
	if err == nil {
		return nil
	}
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return err
	}

	cleanupCtx, cancel := context.WithTimeout(context.Background(), startupCleanupTimeout)
	defer cancel()

	if destroyErr := m.mesh.Destroy(cleanupCtx); destroyErr != nil {
		return errors.Join(err, fmt.Errorf("destroy partial mesh: %w", destroyErr))
	}
	return err
}

func (m *Machine) shutdown(ctx context.Context) {
	if m.mesh == nil {
		return
	}
	if err := m.mesh.Detach(ctx); err != nil {
		slog.Error("mesh shutdown", "err", err)
	}
}
