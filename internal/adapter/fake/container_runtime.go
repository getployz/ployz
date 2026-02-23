package fake

import (
	"context"
	"fmt"
	"net/netip"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/mesh"
)

var _ mesh.ContainerRuntime = (*ContainerRuntime)(nil)

const (
	FaultContainerRuntimeWaitReady      = "container_runtime.wait_ready"
	FaultContainerRuntimeInspect        = "container_runtime.container_inspect"
	FaultContainerRuntimeStart          = "container_runtime.container_start"
	FaultContainerRuntimeStop           = "container_runtime.container_stop"
	FaultContainerRuntimeRemove         = "container_runtime.container_remove"
	FaultContainerRuntimeLogs           = "container_runtime.container_logs"
	FaultContainerRuntimeCreate         = "container_runtime.container_create"
	FaultContainerRuntimeList           = "container_runtime.container_list"
	FaultContainerRuntimeUpdate         = "container_runtime.container_update"
	FaultContainerRuntimeImagePull      = "container_runtime.image_pull"
	FaultContainerRuntimeNetworkInspect = "container_runtime.network_inspect"
	FaultContainerRuntimeNetworkCreate  = "container_runtime.network_create"
	FaultContainerRuntimeNetworkRemove  = "container_runtime.network_remove"
)

type containerState struct {
	Config  mesh.ContainerCreateConfig
	Running bool
}

type networkState struct {
	ID     string
	Subnet string
}

// ContainerRuntime is an in-memory implementation of mesh.ContainerRuntime.
type ContainerRuntime struct {
	CallRecorder
	mu         sync.Mutex
	ready      bool
	containers map[string]*containerState
	networks   map[string]*networkState
	images     map[string]bool
	faults     *fault.Injector

	// LogsOutput is returned by ContainerLogs when non-empty,
	// allowing tests to inject canned log output.
	LogsOutput string

	WaitReadyErr        func(ctx context.Context) error
	ContainerInspectErr func(ctx context.Context, name string) error
	ContainerStartErr   func(ctx context.Context, name string) error
	ContainerStopErr    func(ctx context.Context, name string) error
	ContainerRemoveErr  func(ctx context.Context, name string, force bool) error
	ContainerLogsErr    func(ctx context.Context, name string, lines int) error
	ContainerCreateErr  func(ctx context.Context, cfg mesh.ContainerCreateConfig) error
	ContainerListErr    func(ctx context.Context, labelFilter map[string]string) error
	ContainerUpdateErr  func(ctx context.Context, name string, resources mesh.ResourceConfig) error
	ImagePullErr        func(ctx context.Context, image string) error
	NetworkInspectErr   func(ctx context.Context, name string) error
	NetworkCreateErr    func(ctx context.Context, name string, subnet netip.Prefix, wgIface string) error
	NetworkRemoveErr    func(ctx context.Context, name string) error
}

// NewContainerRuntime creates a ContainerRuntime that is ready by default.
func NewContainerRuntime() *ContainerRuntime {
	return &ContainerRuntime{
		ready:      true,
		containers: make(map[string]*containerState),
		networks:   make(map[string]*networkState),
		images:     make(map[string]bool),
		faults:     fault.NewInjector(),
	}
}

func (r *ContainerRuntime) FailOnce(point string, err error) {
	r.faults.FailOnce(point, err)
}

func (r *ContainerRuntime) FailAlways(point string, err error) {
	r.faults.FailAlways(point, err)
}

func (r *ContainerRuntime) SetFaultHook(point string, hook fault.Hook) {
	r.faults.SetHook(point, hook)
}

func (r *ContainerRuntime) ClearFault(point string) {
	r.faults.Clear(point)
}

func (r *ContainerRuntime) ResetFaults() {
	r.faults.Reset()
}

func (r *ContainerRuntime) evalFault(point string, args ...any) error {
	check.Assert(r != nil, "ContainerRuntime.evalFault: receiver must not be nil")
	check.Assert(r.faults != nil, "ContainerRuntime.evalFault: faults injector must not be nil")
	if r == nil || r.faults == nil {
		return nil
	}
	return r.faults.Eval(point, args...)
}

func (r *ContainerRuntime) WaitReady(ctx context.Context) error {
	r.record("WaitReady")
	if err := r.evalFault(FaultContainerRuntimeWaitReady, ctx); err != nil {
		return err
	}
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

func (r *ContainerRuntime) ContainerInspect(ctx context.Context, name string) (mesh.ContainerInfo, error) {
	r.record("ContainerInspect", name)
	if err := r.evalFault(FaultContainerRuntimeInspect, ctx, name); err != nil {
		return mesh.ContainerInfo{}, err
	}
	if r.ContainerInspectErr != nil {
		if err := r.ContainerInspectErr(ctx, name); err != nil {
			return mesh.ContainerInfo{}, err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	cs, ok := r.containers[name]
	if !ok {
		return mesh.ContainerInfo{Exists: false}, nil
	}
	return mesh.ContainerInfo{Exists: true, Running: cs.Running}, nil
}

func (r *ContainerRuntime) ContainerStart(ctx context.Context, name string) error {
	r.record("ContainerStart", name)
	if err := r.evalFault(FaultContainerRuntimeStart, ctx, name); err != nil {
		return err
	}
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
	if err := r.evalFault(FaultContainerRuntimeStop, ctx, name); err != nil {
		return err
	}
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
	if err := r.evalFault(FaultContainerRuntimeRemove, ctx, name, force); err != nil {
		return err
	}
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
	if err := r.evalFault(FaultContainerRuntimeLogs, ctx, name, lines); err != nil {
		return "", err
	}
	if r.ContainerLogsErr != nil {
		if err := r.ContainerLogsErr(ctx, name, lines); err != nil {
			return "", err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.LogsOutput, nil
}

func (r *ContainerRuntime) ContainerCreate(ctx context.Context, cfg mesh.ContainerCreateConfig) error {
	r.record("ContainerCreate", cfg)
	if err := r.evalFault(FaultContainerRuntimeCreate, ctx, cfg); err != nil {
		return err
	}
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

func (r *ContainerRuntime) ContainerList(ctx context.Context, labelFilter map[string]string) ([]mesh.ContainerListEntry, error) {
	r.record("ContainerList", labelFilter)
	if err := r.evalFault(FaultContainerRuntimeList, ctx, labelFilter); err != nil {
		return nil, err
	}
	if r.ContainerListErr != nil {
		if err := r.ContainerListErr(ctx, labelFilter); err != nil {
			return nil, err
		}
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	out := make([]mesh.ContainerListEntry, 0, len(r.containers))
	for name, state := range r.containers {
		if !matchLabels(state.Config.Labels, labelFilter) {
			continue
		}
		labels := make(map[string]string, len(state.Config.Labels))
		for key, value := range state.Config.Labels {
			labels[key] = value
		}
		out = append(out, mesh.ContainerListEntry{
			Name:    name,
			Image:   state.Config.Image,
			Running: state.Running,
			Labels:  labels,
		})
	}
	return out, nil
}

func (r *ContainerRuntime) ContainerUpdate(ctx context.Context, name string, resources mesh.ResourceConfig) error {
	r.record("ContainerUpdate", name, resources)
	if err := r.evalFault(FaultContainerRuntimeUpdate, ctx, name, resources); err != nil {
		return err
	}
	if r.ContainerUpdateErr != nil {
		if err := r.ContainerUpdateErr(ctx, name, resources); err != nil {
			return err
		}
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	if _, ok := r.containers[name]; !ok {
		return fmt.Errorf("container %q not found", name)
	}

	return nil
}

func (r *ContainerRuntime) ImagePull(ctx context.Context, image string) error {
	r.record("ImagePull", image)
	if err := r.evalFault(FaultContainerRuntimeImagePull, ctx, image); err != nil {
		return err
	}
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

func (r *ContainerRuntime) NetworkInspect(ctx context.Context, name string) (mesh.NetworkInfo, error) {
	r.record("NetworkInspect", name)
	if err := r.evalFault(FaultContainerRuntimeNetworkInspect, ctx, name); err != nil {
		return mesh.NetworkInfo{}, err
	}
	if r.NetworkInspectErr != nil {
		if err := r.NetworkInspectErr(ctx, name); err != nil {
			return mesh.NetworkInfo{}, err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	ns, ok := r.networks[name]
	if !ok {
		return mesh.NetworkInfo{Exists: false}, nil
	}
	return mesh.NetworkInfo{ID: ns.ID, Subnet: ns.Subnet, Exists: true}, nil
}

func (r *ContainerRuntime) NetworkCreate(ctx context.Context, name string, subnet netip.Prefix, wgIface string) error {
	r.record("NetworkCreate", name, subnet, wgIface)
	if err := r.evalFault(FaultContainerRuntimeNetworkCreate, ctx, name, subnet, wgIface); err != nil {
		return err
	}
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
	if err := r.evalFault(FaultContainerRuntimeNetworkRemove, ctx, name); err != nil {
		return err
	}
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

func matchLabels(have, want map[string]string) bool {
	for key, value := range want {
		if have[key] != value {
			return false
		}
	}
	return true
}

// SetReady controls whether WaitReady succeeds.
func (r *ContainerRuntime) SetReady(ready bool) {
	r.mu.Lock()
	r.ready = ready
	r.mu.Unlock()
}
