package docker

import (
	"context"
	"fmt"
	"time"

	"github.com/docker/docker/client"
)

func WaitReady(ctx context.Context, cli *client.Client) error {
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			_, err := cli.Ping(ctx)
			if err == nil {
				return nil
			}
			if !client.IsErrConnectionFailed(err) {
				return fmt.Errorf("connect to docker daemon: %w", err)
			}
		}
	}
}
