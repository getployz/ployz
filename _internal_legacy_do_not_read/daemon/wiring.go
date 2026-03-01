package daemon

import (
	"context"

	"ployz/internal/daemon/api"
	"ployz/internal/daemon/manager"
)

func Wire(ctx context.Context, dataRoot string) (*api.Server, error) {
	mgr, err := manager.NewProduction(ctx, dataRoot)
	if err != nil {
		return nil, err
	}
	return api.New(mgr), nil
}
