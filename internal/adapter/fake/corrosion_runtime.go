package fake

import (
	"context"
	"sync"

	"ployz/internal/adapter/fake/fault"
	"ployz/internal/check"
	"ployz/internal/mesh"
)

var _ mesh.CorrosionRuntime = (*CorrosionRuntime)(nil)

const (
	FaultCorrosionWriteConfig = "corrosion_runtime.write_config"
	FaultCorrosionStart       = "corrosion_runtime.start"
	FaultCorrosionStop        = "corrosion_runtime.stop"
)

// CorrosionRuntime is an in-memory implementation of mesh.CorrosionRuntime.
type CorrosionRuntime struct {
	CallRecorder
	mu         sync.Mutex
	LastConfig mesh.CorrosionConfig
	Running    map[string]bool
	faults     *fault.Injector

	WriteConfigErr func(cfg mesh.CorrosionConfig) error
	StartErr       func(ctx context.Context, cfg mesh.CorrosionConfig) error
	StopErr        func(ctx context.Context, name string) error
}

// NewCorrosionRuntime creates a CorrosionRuntime with no containers.
func NewCorrosionRuntime() *CorrosionRuntime {
	return &CorrosionRuntime{Running: make(map[string]bool), faults: fault.NewInjector()}
}

func (r *CorrosionRuntime) FailOnce(point string, err error) {
	r.faults.FailOnce(point, err)
}

func (r *CorrosionRuntime) FailAlways(point string, err error) {
	r.faults.FailAlways(point, err)
}

func (r *CorrosionRuntime) SetFaultHook(point string, hook fault.Hook) {
	r.faults.SetHook(point, hook)
}

func (r *CorrosionRuntime) ClearFault(point string) {
	r.faults.Clear(point)
}

func (r *CorrosionRuntime) ResetFaults() {
	r.faults.Reset()
}

func (r *CorrosionRuntime) evalFault(point string, args ...any) error {
	check.Assert(r != nil, "CorrosionRuntime.evalFault: receiver must not be nil")
	check.Assert(r.faults != nil, "CorrosionRuntime.evalFault: faults injector must not be nil")
	if r == nil || r.faults == nil {
		return nil
	}
	return r.faults.Eval(point, args...)
}

func (r *CorrosionRuntime) WriteConfig(cfg mesh.CorrosionConfig) error {
	r.record("WriteConfig", cfg)
	if err := r.evalFault(FaultCorrosionWriteConfig, cfg); err != nil {
		return err
	}
	if r.WriteConfigErr != nil {
		if err := r.WriteConfigErr(cfg); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	r.LastConfig = cfg
	return nil
}

func (r *CorrosionRuntime) Start(ctx context.Context, cfg mesh.CorrosionConfig) error {
	r.record("Start", cfg)
	if err := r.evalFault(FaultCorrosionStart, ctx, cfg); err != nil {
		return err
	}
	if r.StartErr != nil {
		if err := r.StartErr(ctx, cfg); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	r.LastConfig = cfg
	r.Running[cfg.Name] = true
	return nil
}

func (r *CorrosionRuntime) Stop(ctx context.Context, name string) error {
	r.record("Stop", name)
	if err := r.evalFault(FaultCorrosionStop, ctx, name); err != nil {
		return err
	}
	if r.StopErr != nil {
		if err := r.StopErr(ctx, name); err != nil {
			return err
		}
	}
	r.mu.Lock()
	defer r.mu.Unlock()

	r.Running[name] = false
	return nil
}
