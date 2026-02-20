package dockerutil

import (
	"context"
	"fmt"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
)

func PurgeNetworkContainers(ctx context.Context, cli *client.Client, networkName string, nw dockernetwork.Inspect) error {
	for id := range nw.Containers {
		if err := cli.ContainerRemove(ctx, id, container.RemoveOptions{Force: true, RemoveVolumes: true}); err != nil {
			if errdefs.IsNotFound(err) {
				continue
			}
			return fmt.Errorf("remove container %q attached to docker network %q: %w", id, networkName, err)
		}
	}
	return nil
}
