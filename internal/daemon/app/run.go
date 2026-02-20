package app

import (
	"context"

	"ployz/internal/daemon/server"
	"ployz/internal/daemon/supervisor"
)

func Run(ctx context.Context, socketPath, dataRoot string) error {
	mgr, err := supervisor.New(ctx, dataRoot)
	if err != nil {
		return err
	}
	srv := server.New(mgr)
	return srv.ListenAndServe(ctx, socketPath)
}
