package corrorun

import (
	"context"
	"fmt"
	"log/slog"
	"net/netip"
)

// Service assumes Corrosion is managed externally (systemd, etc.).
// Start only health-checks; Stop is a no-op.
// Implements machine.StoreRuntime.
type Service struct {
	apiAddr netip.AddrPort
}

// NewService creates a runtime that expects Corrosion to be running externally.
func NewService(apiAddr netip.AddrPort) *Service {
	return &Service{apiAddr: apiAddr}
}

// Start waits for the external Corrosion instance to become ready.
func (s *Service) Start(ctx context.Context) error {
	if err := WaitReady(ctx, s.apiAddr); err != nil {
		return fmt.Errorf("external corrosion not ready: %w", err)
	}
	slog.Info("Connected to external Corrosion.", "addr", s.apiAddr)
	return nil
}

// Stop is a no-op — the external service manages its own lifecycle.
func (s *Service) Stop(ctx context.Context) error {
	return nil
}
