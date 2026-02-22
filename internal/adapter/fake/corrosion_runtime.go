package fake

import (
	"context"
	"sync"

	"ployz/internal/mesh"
)

var _ mesh.CorrosionRuntime = (*CorrosionRuntime)(nil)

// CorrosionRuntime is an in-memory implementation of mesh.CorrosionRuntime.
type CorrosionRuntime struct {
	CallRecorder
	mu         sync.Mutex
	LastConfig mesh.CorrosionConfig
	Running    map[string]bool

	WriteConfigErr func(cfg mesh.CorrosionConfig) error
	StartErr       func(ctx context.Context, cfg mesh.CorrosionConfig) error
	StopErr        func(ctx context.Context, name string) error
}

// NewCorrosionRuntime creates a CorrosionRuntime with no containers.
func NewCorrosionRuntime() *CorrosionRuntime {
	return &CorrosionRuntime{Running: make(map[string]bool)}
}

func (r *CorrosionRuntime) WriteConfig(cfg mesh.CorrosionConfig) error {
	r.record("WriteConfig", cfg)
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
