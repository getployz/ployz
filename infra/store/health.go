package store

import (
	"context"

	"ployz/infra/corrosion"
)

// Healthy returns true when Corrosion reports gaps=0.
func (s *Store) Healthy(ctx context.Context) (bool, error) {
	_, ok, err := s.client.HealthContext(ctx, corrosion.HealthThresholds{Gaps: 0})
	return ok, err
}
