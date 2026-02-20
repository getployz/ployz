package daemon

import (
	"context"

	"ployz/internal/daemon/app"
)

func Run(ctx context.Context, socketPath, dataRoot string) error {
	return app.Run(ctx, socketPath, dataRoot)
}
