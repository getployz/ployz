package fake

import (
	"context"
	"fmt"
	"net/netip"
	"sync"

	"ployz/internal/network"
)

var _ network.ContainerRuntime = (*ContainerRuntime)(nil)

type containerState struct {
	Config  network.ContainerCreateConfig
	Running bool
}

type networkState struct {
	ID     string
	Subnet string
}

// ContainerRuntime is an in-memory implementation of network.ContainerRuntime.
type ContainerRuntime struct {
	CallRecorder
	mu         sync.Mutex
	ready      bool
	containers map[string]*containerState
	networks   map[string]*networkState
	images     map[string]bool

	WaitReadyErr      func(ctx context.Context) error
	ContainerInspectErr func(ctx context.Context, name string) error
	ContainerStartErr func(ctx context.Context, name string) error
	ContainerStopErr  func(ctx context.Context, name string) error
	ContainerRemoveErr func(ctx context.Context, name string, force bool) error
	ContainerLogsErr  func(ctx context.Context, name string, lines int) error
	ContainerCreateErr func(ctx context.Context, cfg network.ContainerCreateConfig) error
	ImagePullErr      func(ctx context.Context, image string) error
	NetworkInspectErr func(ctx context.Context, name string) error
	NetworkCreateErr  func(ctx context.Context, name string, subnet netip.Prefix, wgIface string) error
	NetworkRemoveErr  func(ctx context.Context, name string) error
}

// NewContainerRuntime creates a ContainerRuntime that is ready by default.
func NewContainerRuntime() *ContainerRuntime {
	return &ContainerRuntime{
		ready:      true,
		containers: make(map[string]*containerState),
		networks:   make(map[string]*networkState),
		images:     make(map[string]bool),
	}
}

func (r *ContainerRuntime) WaitReady(ctx context.Context) error {
	r.record("WaitReady")
	if r.WaitReadyErr != nil {
		if err := r.WaitReadyErr(ctx); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.ready {
		return fmt.Errorf("container runtime not ready")
	}
	return nil
}

func (r *ContainerRuntime) ContainerInspect(ctx context.Context, name string) (network.ContainerInfo, error) {
	r.record("ContainerInspect", name)
	if r.ContainerInspectErr != nil {
		if err := r.ContainerInspectErr(ctx, name); err != nil {
			return network.ContainerInfo{}, err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	cs, ok := r.containers[name]
	if !ok {
		return network.ContainerInfo{Exists: false}, nil
	}
	return network.ContainerInfo{Exists: true, Running: cs.Running}, nil
}

func (r *ContainerRuntime) ContainerStart(ctx context.Context, name string) error {
	r.record("ContainerStart", name)
	if r.ContainerStartErr != nil {
		if err := r.ContainerStartErr(ctx, name); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	cs, ok := r.containers[name]
	if !ok {
		return fmt.Errorf("container %q not found", name)
	}
	cs.Running = true
	return nil
}

func (r *ContainerRuntime) ContainerStop(ctx context.Context, name string) error {
	r.record("ContainerStop", name)
	if r.ContainerStopErr != nil {
		if err := r.ContainerStopErr(ctx, name); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	cs, ok := r.containers[name]
	if !ok {
		return fmt.Errorf("container %q not found", name)
	}
	cs.Running = false
	return nil
}

func (r *ContainerRuntime) ContainerRemove(ctx context.Context, name string, force bool) error {
	r.record("ContainerRemove", name, force)
	if r.ContainerRemoveErr != nil {
		if err := r.ContainerRemoveErr(ctx, name, force); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	cs, ok := r.containers[name]
	if !ok {
		return nil
	}
	if cs.Running && !force {
		return fmt.Errorf("container %q is running, use force to remove", name)
	}
	delete(r.containers, name)
	return nil
}

func (r *ContainerRuntime) ContainerLogs(ctx context.Context, name string, lines int) (string, error) {
	r.record("ContainerLogs", name, lines)
	if r.ContainerLogsErr != nil {
		if err := r.ContainerLogsErr(ctx, name, lines); err != nil {
			return "", err
		}
	}
	// TODO: ContainerLogs canned output support
	return "", nil
}

func (r *ContainerRuntime) ContainerCreate(ctx context.Context, cfg network.ContainerCreateConfig) error {
	r.record("ContainerCreate", cfg)
	if r.ContainerCreateErr != nil {
		if err := r.ContainerCreateErr(ctx, cfg); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	r.containers[cfg.Name] = &containerState{Config: cfg, Running: false}
	return nil
}

func (r *ContainerRuntime) ImagePull(ctx context.Context, image string) error {
	r.record("ImagePull", image)
	if r.ImagePullErr != nil {
		if err := r.ImagePullErr(ctx, image); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	r.images[image] = true
	return nil
}

func (r *ContainerRuntime) NetworkInspect(ctx context.Context, name string) (network.NetworkInfo, error) {
	r.record("NetworkInspect", name)
	if r.NetworkInspectErr != nil {
		if err := r.NetworkInspectErr(ctx, name); err != nil {
			return network.NetworkInfo{}, err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	ns, ok := r.networks[name]
	if !ok {
		return network.NetworkInfo{Exists: false}, nil
	}
	return network.NetworkInfo{ID: ns.ID, Subnet: ns.Subnet, Exists: true}, nil
}

func (r *ContainerRuntime) NetworkCreate(ctx context.Context, name string, subnet netip.Prefix, wgIface string) error {
	r.record("NetworkCreate", name, subnet, wgIface)
	if r.NetworkCreateErr != nil {
		if err := r.NetworkCreateErr(ctx, name, subnet, wgIface); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	r.networks[name] = &networkState{
		ID:     fmt.Sprintf("fake-%s", name),
		Subnet: subnet.String(),
	}
	return nil
}

func (r *ContainerRuntime) NetworkRemove(ctx context.Context, name string) error {
	r.record("NetworkRemove", name)
	if r.NetworkRemoveErr != nil {
		if err := r.NetworkRemoveErr(ctx, name); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	delete(r.networks, name)
	return nil
}

func (r *ContainerRuntime) Close() error {
	r.record("Close")
	return nil
}

// SetReady controls whether WaitReady succeeds.
func (r *ContainerRuntime) SetReady(ready bool) {
	r.mu.Lock()
	r.ready = ready
	r.mu.Unlock()
}
