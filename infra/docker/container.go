// Package docker provides shared helpers for managing Docker containers.
package docker

import (
	"context"
	"fmt"
	"io"
	"log/slog"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
)

// CreateAndStart creates a Docker container and starts it. If the image is
// not found locally, it pulls the image and retries the create.
func CreateAndStart(
	ctx context.Context,
	docker client.APIClient,
	name, img string,
	containerCfg *container.Config,
	hostCfg *container.HostConfig,
	networkCfg *network.NetworkingConfig,
) error {
	_, err := docker.ContainerCreate(ctx, containerCfg, hostCfg, networkCfg, (*ocispec.Platform)(nil), name)
	if err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("create container: %w", err)
		}
		if err := PullImage(ctx, docker, img); err != nil {
			return err
		}
		if _, err = docker.ContainerCreate(ctx, containerCfg, hostCfg, networkCfg, nil, name); err != nil {
			return fmt.Errorf("create container after pull: %w", err)
		}
	}

	if err := docker.ContainerStart(ctx, name, container.StartOptions{}); err != nil {
		return fmt.Errorf("start container: %w", err)
	}
	return nil
}

// PullImage pulls a Docker image and drains the response to completion.
func PullImage(ctx context.Context, docker client.APIClient, img string) error {
	slog.Info("Pulling image.", "image", img)
	resp, err := docker.ImagePull(ctx, img, image.PullOptions{})
	if err != nil {
		return fmt.Errorf("pull image %s: %w", img, err)
	}
	defer resp.Close()
	if _, err := io.Copy(io.Discard, resp); err != nil {
		return fmt.Errorf("pull image %s: read response: %w", img, err)
	}
	return nil
}

// StopAndRemove stops and removes a container. Both operations are
// idempotent — NotFound errors are silently ignored.
func StopAndRemove(ctx context.Context, docker client.APIClient, name string) error {
	if err := docker.ContainerStop(ctx, name, container.StopOptions{}); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("stop container %s: %w", name, err)
		}
	}
	if err := docker.ContainerRemove(ctx, name, container.RemoveOptions{Force: true}); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("remove container %s: %w", name, err)
		}
	}
	return nil
}
