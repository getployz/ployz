package machine

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"

	"ployz"
)

const startupCleanupTimeout = 15 * time.Second

// Run starts the machine. If a network config exists from a previous init/join,
// the mesh is built (if a builder is available) and started automatically.
// Then blocks until ctx is cancelled.
func (m *Machine) Run(ctx context.Context) error {
	if m.mesh == nil && m.buildMesh != nil && m.hasNetworkConfig() {
		msh, err := m.buildMesh(ctx, m.identity)
		if err != nil {
			return fmt.Errorf("build mesh from config: %w", err)
		}
		m.mesh = msh
	}

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

// InitNetwork creates a new network. It builds a mesh via the builder,
// persists the network config, starts the mesh, and registers the local
// machine in the store.
func (m *Machine) InitNetwork(ctx context.Context, name string) error {
	if m.mesh != nil {
		return fmt.Errorf("network already running (phase %s)", m.Phase())
	}
	if m.buildMesh == nil {
		return fmt.Errorf("no mesh builder configured")
	}

	msh, err := m.buildMesh(ctx, m.identity)
	if err != nil {
		return fmt.Errorf("create mesh: %w", err)
	}

	if err := m.SaveNetworkConfig(NetworkConfig{Network: name}); err != nil {
		return fmt.Errorf("save network config: %w", err)
	}

	m.mesh = msh

	if err := m.startMesh(ctx); err != nil {
		m.mesh = nil
		_ = m.RemoveNetworkConfig()
		return fmt.Errorf("start mesh: %w", err)
	}

	// Register self so other machines can discover us.
	if s := m.Store(); s != nil {
		pub := m.identity.PrivateKey.PublicKey()
		rec := ployz.MachineRecord{
			ID:        pub.String(),
			Name:      m.identity.Name,
			PublicKey:  pub,
			OverlayIP: ployz.ManagementIPFromKey(pub),
			UpdatedAt: time.Now(),
		}
		if err := s.UpsertMachine(ctx, rec); err != nil {
			slog.Error("Failed to register self in store.", "err", err)
		}
	}

	slog.Info("Network initialized.", "name", name)
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
