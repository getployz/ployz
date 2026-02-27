package membership

import (
	"context"
	"fmt"

	"ployz/internal/support/check"
	"ployz/internal/daemon/overlay"
)

type Service struct {
	controller Controller
}

func New(controller Controller) *Service {
	check.Assert(controller != nil, "membership.New: controller must not be nil")
	return &Service{controller: controller}
}

func (s *Service) Close() error {
	if s == nil || s.controller == nil {
		return nil
	}
	return s.controller.Close()
}

func (s *Service) Reconcile(ctx context.Context, cfg overlay.Config) (int, error) {
	if s == nil || s.controller == nil {
		return 0, fmt.Errorf("membership service is not configured")
	}
	return s.controller.Reconcile(ctx, cfg)
}

func (s *Service) ReconcilePeers(ctx context.Context, cfg overlay.Config, rows []MachineRow) (int, error) {
	if s == nil || s.controller == nil {
		return 0, fmt.Errorf("membership service is not configured")
	}
	return s.controller.ReconcilePeers(ctx, cfg, rows)
}
