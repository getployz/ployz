package cmdutil

import (
	"context"
	"fmt"
	"path/filepath"
	"time"

	"ployz/pkg/sdk/client"
)

func IsDaemonRunning(_ context.Context, socketPath string) bool {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	return HealthCheck(ctx, socketPath) == nil
}

func DaemonLogPath(dataRoot string) string {
	return filepath.Join(dataRoot, "ployzd.log")
}

func HealthCheck(ctx context.Context, socketPath string) error {
	checkCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()

	api, err := client.NewUnix(socketPath)
	if err != nil {
		return fmt.Errorf("connect to daemon: %w", err)
	}
	defer func() { _ = api.Close() }()

	if _, err := api.GetStatus(checkCtx, "default"); err != nil {
		return fmt.Errorf("daemon health check: %w", err)
	}
	return nil
}
