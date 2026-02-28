package machine

import (
	"context"
	"fmt"
	"log/slog"
)

// Run starts the machine. If a network config exists from a previous init/join,
// the network is started automatically. Then blocks until ctx is cancelled.
func (m *Machine) Run(ctx context.Context) error {
	if m.hasNetworkConfig() {
		if err := m.EnableNetwork(ctx); err != nil {
			return fmt.Errorf("start network: %w", err)
		}
		slog.Info("Network started from existing config.")
	}

	close(m.started)

	<-ctx.Done()
	return m.shutdown(context.Background())
}

func (m *Machine) shutdown(ctx context.Context) error {
	if err := m.DisableNetwork(ctx); err != nil {
		slog.Error("disable network during shutdown", "err", err)
	}
	return nil
}

// EnableNetwork starts the network components in order: WireGuard, registry,
// then convergence. Returns on first error, tearing down anything already started.
func (m *Machine) EnableNetwork(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.phase != PhaseStopped {
		return fmt.Errorf("network already %s", m.phase)
	}

	m.phase = PhaseStarting

	if err := m.wireGuard.Up(ctx); err != nil {
		m.phase = PhaseStopped
		return fmt.Errorf("setup wireguard: %w", err)
	}

	if m.storeRuntime != nil {
		if err := m.storeRuntime.Start(ctx); err != nil {
			_ = m.wireGuard.Down(ctx) // best-effort teardown; original error is more useful
			m.phase = PhaseStopped
			return fmt.Errorf("start registry: %w", err)
		}
	}

	if m.convergence != nil {
		if err := m.convergence.Start(ctx); err != nil {
			// best-effort teardown; original error is more useful
			if m.storeRuntime != nil {
				_ = m.storeRuntime.Stop(ctx)
			}
			_ = m.wireGuard.Down(ctx)
			m.phase = PhaseStopped
			return fmt.Errorf("start convergence: %w", err)
		}
	}

	m.phase = PhaseRunning
	return nil
}

// DisableNetwork stops the network components in reverse order.
func (m *Machine) DisableNetwork(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.phase != PhaseRunning {
		return nil
	}

	m.phase = PhaseStopping

	var firstErr error

	if m.convergence != nil {
		if err := m.convergence.Stop(); err != nil && firstErr == nil {
			firstErr = fmt.Errorf("stop convergence: %w", err)
		}
	}

	if m.storeRuntime != nil {
		if err := m.storeRuntime.Stop(ctx); err != nil && firstErr == nil {
			firstErr = fmt.Errorf("stop registry: %w", err)
		}
	}

	if err := m.wireGuard.Down(ctx); err != nil && firstErr == nil {
		firstErr = fmt.Errorf("teardown wireguard: %w", err)
	}

	m.phase = PhaseStopped
	return firstErr
}

