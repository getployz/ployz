package mesh

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"time"
)

const (
	bootstrapPollInterval      = 2 * time.Second
	bootstrapConsecutivePasses = 2
)

// ErrBootstrapTimeout is returned when mesh bootstrap exceeds the configured timeout.
var ErrBootstrapTimeout = errors.New("mesh bootstrap timeout")

// Up brings up the network stack in order: WireGuard, store,
// then convergence. After components start, it gates on bootstrap
// readiness before transitioning to PhaseRunning.
//
// On component failure the original error is returned but
// infrastructure that was already running is left intact — a
// subsequent Up or Destroy can deal with it.
func (m *Mesh) Up(ctx context.Context) error {
	m.mu.Lock()

	if m.phase != PhaseStopped {
		m.mu.Unlock()
		return fmt.Errorf("mesh already %s", m.phase)
	}

	m.phase = PhaseStarting

	if m.wireGuard != nil {
		if err := m.wireGuard.Up(ctx); err != nil {
			m.phase = PhaseStopped
			m.mu.Unlock()
			return fmt.Errorf("wireguard up: %w", err)
		}
	}

	if m.store != nil {
		if err := m.store.Start(ctx); err != nil {
			m.phase = PhaseStopped
			m.mu.Unlock()
			return fmt.Errorf("store start: %w", err)
		}
	}

	if m.convergence != nil {
		if err := m.convergence.Start(ctx); err != nil {
			m.phase = PhaseStopped
			m.mu.Unlock()
			return fmt.Errorf("convergence start: %w", err)
		}
	}

	m.phase = PhaseBootstrapping
	m.mu.Unlock()

	if err := m.waitBootstrap(ctx); err != nil {
		return fmt.Errorf("bootstrap: %w", err)
	}

	m.mu.Lock()
	if m.phase == PhaseBootstrapping {
		m.phase = PhaseRunning
	}
	m.mu.Unlock()
	return nil
}

// waitBootstrap polls convergence health and store health until the mesh
// is ready. The gate is binary:
//   - Has reachable peers → wait for store gaps=0
//   - No reachable peers → ready immediately (nothing to sync)
//
// Both paths require 2 consecutive passes to avoid flap.
func (m *Mesh) waitBootstrap(ctx context.Context) error {
	// Skip entirely when no health sources are configured (tests/stubs).
	if m.convergence == nil && m.storeHealth == nil {
		return nil
	}

	timeout := m.bootstrapTimeout
	if timeout == 0 {
		timeout = defaultBootstrapTimeout
	}

	parentCtx := ctx
	ctx, cancel := context.WithTimeout(parentCtx, timeout)
	defer cancel()

	ticker := time.NewTicker(bootstrapPollInterval)
	defer ticker.Stop()

	consecutivePasses := 0
	for {
		select {
		case <-ctx.Done():
			if err := parentCtx.Err(); err != nil {
				return err
			}
			return fmt.Errorf("%w after %s", ErrBootstrapTimeout, timeout)
		case <-ticker.C:
			pass, err := m.bootstrapPass(ctx)
			if err != nil {
				return err
			}
			if pass {
				consecutivePasses++
				if consecutivePasses >= bootstrapConsecutivePasses {
					return nil
				}
			} else {
				consecutivePasses = 0
			}
		}
	}
}

// bootstrapPass evaluates one bootstrap check cycle.
// Returns true if the mesh should be considered ready this tick.
func (m *Mesh) bootstrapPass(ctx context.Context) (bool, error) {
	if m.convergence == nil {
		// No convergence → pass (stub/test with only storeHealth).
		return true, nil
	}

	health := m.convergence.Health()
	if !health.Initialized {
		slog.Debug("bootstrap: waiting for first probe")
		return false, nil
	}

	if !health.HasReachablePeers() {
		// No one to sync with — single node or all peers suspect.
		slog.Debug("bootstrap: no reachable peers, passing")
		return true, nil
	}

	// Has reachable peers — need store health to confirm sync is complete.
	if m.storeHealth == nil {
		return false, fmt.Errorf("mesh has reachable peers but no store health checker configured")
	}

	ok, err := m.storeHealth.Healthy(ctx)
	if err != nil {
		slog.Warn("bootstrap: store health check failed", "err", err)
		return false, nil
	}
	if !ok {
		slog.Debug("bootstrap: store syncing (gaps > 0)")
		return false, nil
	}

	slog.Debug("bootstrap: store synced (gaps=0)")
	return true, nil
}

// Detach is the control-plane shutdown path. It stops the convergence
// loop but leaves infrastructure (WireGuard interface, Corrosion
// container) running so that workload traffic is unaffected by daemon
// restarts. Use Destroy to tear down infrastructure.
func (m *Mesh) Detach(_ context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.phase != PhaseRunning && m.phase != PhaseBootstrapping {
		return nil
	}

	m.phase = PhaseStopping

	if m.convergence != nil {
		if err := m.convergence.Stop(); err != nil {
			m.phase = PhaseStopped
			return fmt.Errorf("convergence stop: %w", err)
		}
	}

	m.phase = PhaseStopped
	return nil
}

// Destroy tears down the full network stack in reverse order:
// convergence, store, then WireGuard. This is the only path that
// removes infrastructure. Safe to call after a partial Up failure —
// each teardown step is idempotent. Continues through errors and
// returns the first one encountered.
func (m *Mesh) Destroy(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

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
