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

// ReadinessCheck blocks until the store is accepting queries.
type ReadinessCheck func(ctx context.Context, addr netip.AddrPort) error

// Container runs Corrosion as a Docker container.
// Implements the Start/Stop portion of mesh.Store.
type Container struct {
	docker      client.APIClient
	image       string
	name        string
	networkMode container.NetworkMode
	paths       Paths
	apiAddr     netip.AddrPort
	readyCheck  ReadinessCheck
}

// ContainerOption configures a Container runtime.
type ContainerOption func(*Container)

func WithImage(img string) ContainerOption {
	return func(c *Container) { c.image = img }
}

func WithContainerName(name string) ContainerOption {
	return func(c *Container) { c.name = name }
}

// WithNetworkMode overrides the Docker network mode. Defaults to "host".
// On macOS, pass the shared mesh network name (e.g. "ployz-mesh") so
// Corrosion joins the same bridge network as the WireGuard container.
func WithNetworkMode(mode container.NetworkMode) ContainerOption {
	return func(c *Container) { c.networkMode = mode }
}

// WithReadinessCheck overrides the default readiness check (WaitReady).
func WithReadinessCheck(fn ReadinessCheck) ContainerOption {
	return func(c *Container) { c.readyCheck = fn }
}

// NewContainer creates a Docker-based Corrosion runtime.
func NewContainer(docker client.APIClient, paths Paths, apiAddr netip.AddrPort, opts ...ContainerOption) *Container {
	c := &Container{
		docker:      docker,
		image:       DefaultImage,
		name:        DefaultContainerName,
		networkMode: "host",
		paths:       paths,
		apiAddr:     apiAddr,
		readyCheck:  WaitReady,
	}
	for _, opt := range opts {
		opt(c)
	}
	return c
}

// Start ensures the Corrosion container is running and ready. If the
// container already exists and is running it reconnects without
// restarting. If it exists but is stopped it starts it. Only when no
// container exists does it create one from scratch.
func (c *Container) Start(ctx context.Context) error {
	info, err := c.docker.ContainerInspect(ctx, c.name)
	if err == nil {
		// Container exists.
		if info.State.Running {
			slog.Info("Reusing running Corrosion container.", "name", c.name)
			return c.readyCheck(ctx, c.apiAddr)
		}

		// Exists but stopped — start it.
		if err := c.docker.ContainerStart(ctx, c.name, container.StartOptions{}); err != nil {
			return fmt.Errorf("start existing corrosion container: %w", err)
		}
		slog.Info("Started existing Corrosion container.", "name", c.name)
		return c.readyCheck(ctx, c.apiAddr)
	}

	if !errdefs.IsNotFound(err) {
		return fmt.Errorf("inspect corrosion container: %w", err)
	}

	// Container doesn't exist — create from scratch.
	if err := c.createAndStart(ctx); err != nil {
		return fmt.Errorf("start corrosion container: %w", err)
	}

	if err := c.readyCheck(ctx, c.apiAddr); err != nil {
		return err
	}

	slog.Info("Corrosion container started.", "name", c.name)
	return nil
}

// Stop stops and removes the container. This is only called from
// Mesh.Destroy — daemon shutdown leaves the container running.
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

func (c *Container) createAndStart(ctx context.Context) error {
	containerCfg := &container.Config{
		Image: c.image,
		Cmd:   []string{"corrosion", "agent", "-c", c.paths.Config},
	}
	hostCfg := &container.HostConfig{
		NetworkMode: c.networkMode,
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
