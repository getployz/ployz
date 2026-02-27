package membership

import (
	"context"
	"fmt"

	"ployz/internal/daemon/overlay"
)

func (s *Service) ListMachines(ctx context.Context, cfg overlay.Config) ([]Machine, error) {
	if s == nil || s.controller == nil {
		return nil, fmt.Errorf("membership service is not configured")
	}
	return s.controller.ListMachines(ctx, cfg)
}

func (s *Service) UpsertMachine(ctx context.Context, cfg overlay.Config, machine Machine) error {
	if s == nil || s.controller == nil {
		return fmt.Errorf("membership service is not configured")
	}
	return s.controller.UpsertMachine(ctx, cfg, machine)
}

func (s *Service) RemoveMachine(ctx context.Context, cfg overlay.Config, machineID string) error {
	if s == nil || s.controller == nil {
		return fmt.Errorf("membership service is not configured")
	}
	return s.controller.RemoveMachine(ctx, cfg, machineID)
}
