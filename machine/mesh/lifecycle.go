package mesh

import (
	"context"
	"fmt"
	"log/slog"
)

// Start brings up the network stack in order: WireGuard, store runtime,
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

	if m.storeRuntime != nil {
		if err := m.storeRuntime.Start(ctx); err != nil {
			m.teardownFrom(ctx, 0) // tear down WG
			return fmt.Errorf("store runtime start: %w", err)
		}
	}

	if m.convergence != nil {
		if err := m.convergence.Start(ctx); err != nil {
			m.teardownFrom(ctx, 1) // tear down runtime + WG
			return fmt.Errorf("convergence start: %w", err)
		}
	}

	m.phase = PhaseRunning
	return nil
}

// Stop tears down the network stack in reverse order: convergence,
// store runtime, then WireGuard. Continues through errors and returns
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

	if m.storeRuntime != nil {
		if err := m.storeRuntime.Stop(ctx); err != nil && firstErr == nil {
			firstErr = fmt.Errorf("store runtime stop: %w", err)
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

// teardownFrom tears down components in reverse from the given step (inclusive).
// Called during Start rollback. Caller must hold m.mu.
//
// Steps: 0=WG, 1=runtime, 2=convergence.
func (m *Mesh) teardownFrom(ctx context.Context, lastSuccessful int) {
	if lastSuccessful >= 1 && m.storeRuntime != nil {
		if err := m.storeRuntime.Stop(ctx); err != nil {
			slog.Error("rollback: store runtime stop", "err", err)
		}
	}
	if lastSuccessful >= 0 && m.wireGuard != nil {
		if err := m.wireGuard.Down(ctx); err != nil {
			slog.Error("rollback: wireguard down", "err", err)
		}
	}
	m.phase = PhaseStopped
}
