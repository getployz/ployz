package container

import (
	"context"
	"fmt"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
)

// ensureNetwork creates the named Docker bridge network if it does not
// already exist. It is idempotent — calling it when the network is
// already present is a no-op.
func ensureNetwork(ctx context.Context, docker client.APIClient, name string) error {
	_, err := docker.NetworkInspect(ctx, name, network.InspectOptions{})
	if err == nil {
		return nil
	}
	if !errdefs.IsNotFound(err) {
		return fmt.Errorf("inspect docker network %s: %w", name, err)
	}

	_, err = docker.NetworkCreate(ctx, name, network.CreateOptions{
		Driver: "bridge",
	})
	if err != nil {
		return fmt.Errorf("create docker network %s: %w", name, err)
	}
	return nil
}
