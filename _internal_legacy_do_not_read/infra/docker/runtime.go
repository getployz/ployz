package docker

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"log/slog"
	"net/netip"
	"strconv"
	"strings"
	"time"

	"ployz/internal/daemon/overlay"

	"github.com/containerd/errdefs"
	"github.com/docker/docker/api/types/container"
	dockerfilters "github.com/docker/docker/api/types/filters"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/api/types/mount"
	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
	"github.com/docker/go-connections/nat"
)

var _ overlay.ContainerRuntime = (*Runtime)(nil)

// Runtime implements overlay.ContainerRuntime using the Docker Engine API.
type Runtime struct {
	cli *client.Client
}

// NewRuntime creates a Runtime with a new Docker client from the environment.
func NewRuntime() (*Runtime, error) {
	cli, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, fmt.Errorf("create docker client: %w", err)
	}
	return &Runtime{cli: cli}, nil
}

// NewRuntimeFromClient wraps an existing Docker client.
func NewRuntimeFromClient(cli *client.Client) *Runtime {
	return &Runtime{cli: cli}
}

// Client returns the underlying Docker client for callers that still need
// low-level access (e.g. iptables helpers that need network inspect details).
func (r *Runtime) Client() *client.Client {
	return r.cli
}

func (r *Runtime) WaitReady(ctx context.Context) error {
	return WaitReady(ctx, r.cli)
}

func (r *Runtime) ContainerInspect(ctx context.Context, name string) (overlay.ContainerInfo, error) {
	info, err := r.cli.ContainerInspect(ctx, name)
	if err != nil {
		if errdefs.IsNotFound(err) {
			return overlay.ContainerInfo{Exists: false}, nil
		}
		return overlay.ContainerInfo{}, fmt.Errorf("inspect container %q: %w", name, err)
	}
	running := info.State != nil && info.State.Running
	return overlay.ContainerInfo{Exists: true, Running: running}, nil
}

func (r *Runtime) ContainerStart(ctx context.Context, name string) error {
	return r.cli.ContainerStart(ctx, name, container.StartOptions{})
}

func (r *Runtime) ContainerStop(ctx context.Context, name string) error {
	return r.cli.ContainerStop(ctx, name, container.StopOptions{})
}

func (r *Runtime) ContainerRemove(ctx context.Context, name string, force bool) error {
	return r.cli.ContainerRemove(ctx, name, container.RemoveOptions{Force: force})
}

func (r *Runtime) ContainerLogs(ctx context.Context, name string, lines int) (string, error) {
	opts := container.LogsOptions{
		ShowStdout: true,
		ShowStderr: true,
		Tail:       fmt.Sprintf("%d", lines),
	}
	rc, err := r.cli.ContainerLogs(ctx, name, opts)
	if err != nil {
		return "", fmt.Errorf("container logs %q: %w", name, err)
	}
	defer rc.Close()
	data, _ := io.ReadAll(rc)
	// Strip docker stream framing (8-byte header per chunk).
	var clean []byte
	for len(data) >= 8 {
		size := int(data[4])<<24 | int(data[5])<<16 | int(data[6])<<8 | int(data[7])
		data = data[8:]
		if size > len(data) {
			size = len(data)
		}
		clean = append(clean, data[:size]...)
		data = data[size:]
	}
	return string(bytes.TrimSpace(clean)), nil
}

func (r *Runtime) ContainerCreate(ctx context.Context, cfg overlay.ContainerCreateConfig) error {
	cc := &container.Config{
		Image:  cfg.Image,
		Cmd:    cfg.Cmd,
		Env:    cfg.Env,
		User:   cfg.User,
		Labels: cfg.Labels,
	}
	if cfg.HealthCheck != nil {
		cc.Healthcheck = &container.HealthConfig{
			Test:        cfg.HealthCheck.Test,
			Interval:    cfg.HealthCheck.Interval,
			Timeout:     cfg.HealthCheck.Timeout,
			Retries:     cfg.HealthCheck.Retries,
			StartPeriod: cfg.HealthCheck.StartPeriod,
		}
	}

	restartPolicy := container.RestartPolicy{Name: container.RestartPolicyAlways}
	if strings.TrimSpace(cfg.RestartPolicy) != "" {
		restartPolicy = parseRestartPolicy(cfg.RestartPolicy)
	}
	hc := &container.HostConfig{
		NetworkMode:   container.NetworkMode(cfg.NetworkMode),
		RestartPolicy: restartPolicy,
	}

	if len(cfg.Ports) > 0 {
		portBindings := make(nat.PortMap, len(cfg.Ports))
		exposedPorts := make(nat.PortSet, len(cfg.Ports))
		for _, p := range cfg.Ports {
			proto := strings.ToLower(strings.TrimSpace(p.Protocol))
			if proto == "" {
				proto = "tcp"
			}
			containerPort := nat.Port(fmt.Sprintf("%d/%s", p.ContainerPort, proto))
			exposedPorts[containerPort] = struct{}{}
			portBindings[containerPort] = []nat.PortBinding{{HostPort: strconv.Itoa(int(p.HostPort))}}
		}
		cc.ExposedPorts = exposedPorts
		hc.PortBindings = portBindings
	}

	hc.Mounts = make([]mount.Mount, 0, len(cfg.Mounts))
	for _, m := range cfg.Mounts {
		hc.Mounts = append(hc.Mounts, mount.Mount{
			Type:     mount.TypeBind,
			Source:   m.Source,
			Target:   m.Target,
			ReadOnly: m.ReadOnly,
		})
	}
	_, err := r.cli.ContainerCreate(ctx, cc, hc, nil, nil, cfg.Name)
	return err
}

func (r *Runtime) ContainerList(ctx context.Context, labelFilter map[string]string) ([]overlay.ContainerListEntry, error) {
	filters := dockerfilters.NewArgs()
	for key, value := range labelFilter {
		filters.Add("label", key+"="+value)
	}

	containers, err := r.cli.ContainerList(ctx, container.ListOptions{All: true, Filters: filters})
	if err != nil {
		return nil, fmt.Errorf("list containers: %w", err)
	}

	out := make([]overlay.ContainerListEntry, 0, len(containers))
	for _, c := range containers {
		name := ""
		if len(c.Names) > 0 {
			name = strings.TrimPrefix(c.Names[0], "/")
		}

		labels := make(map[string]string, len(c.Labels))
		for key, value := range c.Labels {
			labels[key] = value
		}

		out = append(out, overlay.ContainerListEntry{
			Name:    name,
			Image:   c.Image,
			Running: c.State == "running",
			Labels:  labels,
		})
	}

	return out, nil
}

func (r *Runtime) ContainerUpdate(ctx context.Context, name string, resources overlay.ResourceConfig) error {
	updateConfig := container.UpdateConfig{}
	if resources.CPULimit > 0 {
		updateConfig.Resources.NanoCPUs = int64(resources.CPULimit * 1e9)
	}
	if resources.MemoryLimit > 0 {
		updateConfig.Resources.Memory = resources.MemoryLimit
	}

	_, err := r.cli.ContainerUpdate(ctx, name, updateConfig)
	if err != nil {
		return fmt.Errorf("update container %q resources: %w", name, err)
	}
	return nil
}

func (r *Runtime) ImagePull(ctx context.Context, img string) error {
	pull, err := r.cli.ImagePull(ctx, img, image.PullOptions{})
	if err != nil {
		return fmt.Errorf("pull image %q: %w", img, err)
	}
	_, _ = io.Copy(io.Discard, pull)
	_ = pull.Close()
	return nil
}

func (r *Runtime) NetworkInspect(ctx context.Context, name string) (overlay.NetworkInfo, error) {
	nw, err := r.cli.NetworkInspect(ctx, name, dockernetwork.InspectOptions{})
	if err != nil {
		if errdefs.IsNotFound(err) {
			return overlay.NetworkInfo{Exists: false}, nil
		}
		return overlay.NetworkInfo{}, fmt.Errorf("inspect network %q: %w", name, err)
	}
	var subnet string
	if len(nw.IPAM.Config) > 0 {
		subnet = nw.IPAM.Config[0].Subnet
	}
	return overlay.NetworkInfo{ID: nw.ID, Subnet: subnet, Exists: true}, nil
}

func (r *Runtime) NetworkCreate(ctx context.Context, name string, subnet netip.Prefix, wgIface string) error {
	_, err := r.cli.NetworkCreate(ctx, name, dockernetwork.CreateOptions{
		Driver: "bridge",
		Scope:  "local",
		IPAM:   &dockernetwork.IPAM{Config: []dockernetwork.IPAMConfig{{Subnet: subnet.String()}}},
		Options: map[string]string{
			"com.docker.network.bridge.trusted_host_interfaces": wgIface,
		},
	})
	if err != nil {
		return fmt.Errorf("create network %q: %w", name, err)
	}
	return nil
}

func (r *Runtime) NetworkRemove(ctx context.Context, name string) error {
	if err := r.cli.NetworkRemove(ctx, name); err != nil && !errdefs.IsNotFound(err) {
		return fmt.Errorf("remove network %q: %w", name, err)
	}
	return nil
}

func (r *Runtime) Close() error {
	return r.cli.Close()
}

func parseRestartPolicy(policy string) container.RestartPolicy {
	switch strings.TrimSpace(policy) {
	case "no", "":
		return container.RestartPolicy{Name: container.RestartPolicyDisabled}
	case "always":
		return container.RestartPolicy{Name: container.RestartPolicyAlways}
	case "on-failure":
		return container.RestartPolicy{Name: container.RestartPolicyOnFailure}
	case "unless-stopped":
		return container.RestartPolicy{Name: container.RestartPolicyUnlessStopped}
	default:
		return container.RestartPolicy{Name: container.RestartPolicyAlways}
	}
}

// WaitContainerRemoved polls until a container is removed or timeout.
func WaitContainerRemoved(ctx context.Context, rt *Runtime, name string, timeout time.Duration) error {
	deadline := time.NewTimer(timeout)
	defer deadline.Stop()
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()

	for {
		info, err := rt.ContainerInspect(ctx, name)
		if err != nil {
			slog.Debug("wait container removed inspect error", "container", name, "err", err)
			return nil
		}
		if !info.Exists {
			return nil
		}

		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-deadline.C:
			return fmt.Errorf("timeout after %s waiting for container %q removal", timeout, name)
		}
	}
}
