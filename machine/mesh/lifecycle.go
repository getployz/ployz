package mesh

import (
	"context"
	"fmt"
	"log/slog"
)

// Start brings up the network stack in order: WireGuard, store,
// then convergence. On failure at any step, everything already started
// is torn down in reverse and the original error is returned.
func (m *Mesh) Start(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.phase != PhaseStopped {
		return fmt.Errorf("mesh already %s", m.phase)
	}

	m.phase = PhaseStarting

	if m.wireGuard != nil {
		if err := m.wireGuard.Up(ctx); err != nil {
			m.phase = PhaseStopped
			return fmt.Errorf("wireguard up: %w", err)
		}
	}

	if m.store != nil {
		if err := m.store.Start(ctx); err != nil {
			m.rollbackWireGuard(ctx)
			return fmt.Errorf("store start: %w", err)
		}
	}

	if m.convergence != nil {
		if err := m.convergence.Start(ctx); err != nil {
			m.rollbackStoreAndWireGuard(ctx)
			return fmt.Errorf("convergence start: %w", err)
		}
	}

	m.phase = PhaseRunning
	return nil
}

// Stop tears down the network stack in reverse order: convergence,
// store, then WireGuard. Continues through errors and returns
// the first one encountered.
func (m *Mesh) Stop(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.phase != PhaseRunning {
		return nil
	}

	m.phase = PhaseStopping

	var firstErr error

	if m.convergence != nil {
		if err := m.convergence.Stop(); err != nil {
			firstErr = fmt.Errorf("convergence stop: %w", err)
		}
	}

	if m.store != nil {
		if err := m.store.Stop(ctx); err != nil && firstErr == nil {
			firstErr = fmt.Errorf("store stop: %w", err)
		}
	}

	if m.wireGuard != nil {
		if err := m.wireGuard.Down(ctx); err != nil && firstErr == nil {
			firstErr = fmt.Errorf("wireguard down: %w", err)
		}
	}

	m.phase = PhaseStopped
	return firstErr
}

// rollbackWireGuard tears down WireGuard during a failed Start.
// Caller must hold m.mu.
func (m *Mesh) rollbackWireGuard(ctx context.Context) {
	if m.wireGuard != nil {
		if err := m.wireGuard.Down(ctx); err != nil {
			slog.Error("rollback: wireguard down", "err", err)
		}
	}
	m.phase = PhaseStopped
}

// rollbackStoreAndWireGuard tears down the store and WireGuard
// during a failed Start. Caller must hold m.mu.
func (m *Mesh) rollbackStoreAndWireGuard(ctx context.Context) {
	if m.store != nil {
		if err := m.store.Stop(ctx); err != nil {
			slog.Error("rollback: store stop", "err", err)
		}
	}
	m.rollbackWireGuard(ctx)
}
