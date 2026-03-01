package mesh

import (
	"context"
	"fmt"
)

// Start brings up the network stack in order: WireGuard, store,
// then convergence. On failure the original error is returned but
// infrastructure that was already running is left intact — a
// subsequent Start or Destroy can deal with it.
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
			m.phase = PhaseStopped
			return fmt.Errorf("store start: %w", err)
		}
	}

	if m.convergence != nil {
		if err := m.convergence.Start(ctx); err != nil {
			m.phase = PhaseStopped
			return fmt.Errorf("convergence start: %w", err)
		}
	}

	m.phase = PhaseRunning
	return nil
}

// Stop is the control-plane shutdown path. It stops the convergence
// loop but leaves infrastructure (WireGuard interface, Corrosion
// container) running so that workload traffic is unaffected by daemon
// restarts. Use Destroy to tear down infrastructure.
func (m *Mesh) Stop(_ context.Context) error {
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

	m.phase = PhaseStopped
	return firstErr
}

// Destroy tears down the full network stack in reverse order:
// convergence, store, then WireGuard. This is the only path that
// removes infrastructure. Continues through errors and returns the
// first one encountered.
func (m *Mesh) Destroy(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.phase == PhaseStopped {
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
