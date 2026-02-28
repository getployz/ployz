package corrorun

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net/netip"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/mount"
	"github.com/docker/docker/client"
)

const (
	DefaultImage         = "ghcr.io/superfly/corrosion:latest"
	DefaultContainerName = "ployz-corrosion"
)

// Container runs Corrosion as a Docker container.
// Implements the Start/Stop portion of mesh.Store.
type Container struct {
	docker  client.APIClient
	image   string
	name    string
	paths   Paths
	apiAddr netip.AddrPort
}

// ContainerOption configures a Container runtime.
type ContainerOption func(*Container)

func WithImage(img string) ContainerOption {
	return func(c *Container) { c.image = img }
}

func WithContainerName(name string) ContainerOption {
	return func(c *Container) { c.name = name }
}

// NewContainer creates a Docker-based Corrosion runtime.
func NewContainer(docker client.APIClient, paths Paths, apiAddr netip.AddrPort, opts ...ContainerOption) *Container {
	c := &Container{
		docker:  docker,
		image:   DefaultImage,
		name:    DefaultContainerName,
		paths:   paths,
		apiAddr: apiAddr,
	}
	for _, opt := range opts {
		opt(c)
	}
	return c
}

// Start pulls the image if needed, creates and starts the container,
// then waits for Corrosion to be ready.
func (c *Container) Start(ctx context.Context) error {
	// Remove any existing container.
	_ = c.removeContainer(ctx) // best-effort; may not exist

	if err := c.startContainer(ctx); err != nil {
		return fmt.Errorf("start corrosion container: %w", err)
	}

	if err := WaitReady(ctx, c.apiAddr); err != nil {
		// Try to clean up on failure.
		_ = c.removeContainer(ctx) // best-effort cleanup
		return err
	}

	slog.Info("Corrosion container started.", "name", c.name)
	return nil
}

// Stop stops and removes the container.
func (c *Container) Stop(ctx context.Context) error {
	if err := c.docker.ContainerStop(ctx, c.name, container.StopOptions{}); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("stop corrosion container: %w", err)
		}
	}
	if err := c.docker.ContainerRemove(ctx, c.name, container.RemoveOptions{Force: true}); err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("remove corrosion container: %w", err)
		}
	}
	return nil
}

func (c *Container) startContainer(ctx context.Context) error {
	containerCfg := &container.Config{
		Image: c.image,
		Cmd:   []string{"corrosion", "agent", "-c", c.paths.Config},
	}
	hostCfg := &container.HostConfig{
		NetworkMode: "host",
		RestartPolicy: container.RestartPolicy{
			Name: container.RestartPolicyAlways,
		},
		Mounts: []mount.Mount{
			{
				Type:   mount.TypeBind,
				Source: c.paths.Dir,
				Target: c.paths.Dir,
			},
		},
	}

	_, err := c.docker.ContainerCreate(ctx, containerCfg, hostCfg, nil, nil, c.name)
	if err != nil {
		if !errdefs.IsNotFound(err) {
			return fmt.Errorf("create container: %w", err)
		}
		if err := c.pullImage(ctx); err != nil {
			return err
		}
		if _, err = c.docker.ContainerCreate(ctx, containerCfg, hostCfg, nil, nil, c.name); err != nil {
			return fmt.Errorf("create container after pull: %w", err)
		}
	}

	if err := c.docker.ContainerStart(ctx, c.name, container.StartOptions{}); err != nil {
		return fmt.Errorf("start container: %w", err)
	}
	return nil
}

func (c *Container) pullImage(ctx context.Context) error {
	slog.Info("Pulling Corrosion image.", "image", c.image)
	resp, err := c.docker.ImagePull(ctx, c.image, image.PullOptions{})
	if err != nil {
		return fmt.Errorf("pull corrosion image: %w", err)
	}
	defer resp.Close()
	// Drain the pull output to completion.
	if _, err := io.Copy(io.Discard, resp); err != nil {
		return fmt.Errorf("pull corrosion image: read response: %w", err)
	}
	return nil
}

func (c *Container) removeContainer(ctx context.Context) error {
	return c.docker.ContainerRemove(ctx, c.name, container.RemoveOptions{Force: true})
}
