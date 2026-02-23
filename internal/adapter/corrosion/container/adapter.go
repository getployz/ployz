package container

import (
	"context"

	"ployz/internal/adapter/docker"
	"ployz/internal/mesh"
)

var _ mesh.CorrosionRuntime = (*Adapter)(nil)

// Adapter implements mesh.CorrosionRuntime using the existing container functions
// and a docker.Runtime for container operations.
type Adapter struct {
	rt *docker.Runtime
}

// NewAdapter creates a CorrosionRuntime adapter backed by a docker.Runtime.
func NewAdapter(rt *docker.Runtime) *Adapter {
	return &Adapter{rt: rt}
}

func (a *Adapter) WriteConfig(cfg mesh.CorrosionConfig) error {
	return WriteConfig(Config{
		Dir:          cfg.Dir,
		ConfigPath:   cfg.ConfigPath,
		AdminSock:    cfg.AdminSock,
		Bootstrap:    cfg.Bootstrap,
		GossipAddr:   cfg.GossipAddr,
		MemberID:     cfg.MemberID,
		APIAddr:      cfg.APIAddr,
		APIToken:     cfg.APIToken,
		GossipMaxMTU: cfg.GossipMaxMTU,
		User:         cfg.User,
	})
}

func (a *Adapter) Start(ctx context.Context, cfg mesh.CorrosionConfig) error {
	return Start(ctx, a.rt.Client(), RuntimeConfig{
		Name:       cfg.Name,
		Image:      cfg.Image,
		ConfigPath: cfg.ConfigPath,
		DataDir:    cfg.DataDir,
		User:       cfg.User,
		GossipAddr: cfg.GossipAddr,
		APIAddr:    cfg.APIAddr,
		APIToken:   cfg.APIToken,
	})
}

func (a *Adapter) Stop(ctx context.Context, name string) error {
	return Stop(ctx, a.rt.Client(), name)
}
