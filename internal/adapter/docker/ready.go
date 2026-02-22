package docker

import (
	"context"
	"fmt"
	"log/slog"
	"time"

	"github.com/docker/docker/client"
)

func WaitReady(ctx context.Context, cli *client.Client) error {
	log := slog.With("component", "docker")
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	waiting := false
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-ticker.C:
			_, err := cli.Ping(ctx)
			if err == nil {
				if waiting {
					log.Debug("daemon reachable")
				}
				return nil
			}
			if !client.IsErrConnectionFailed(err) {
				log.Error("ping failed", "err", err)
				return fmt.Errorf("connect to docker daemon: %w", err)
			}
			if !waiting {
				waiting = true
				log.Debug("waiting for docker daemon")
			}
		}
	}
}
