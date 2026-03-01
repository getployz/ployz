package container

import (
	"context"

	"ployz/internal/infra/docker"
	"ployz/internal/daemon/overlay"
)

var _ overlay.CorrosionRuntime = (*Adapter)(nil)

// Adapter implements overlay.CorrosionRuntime using the existing container functions
// and a docker.Runtime for container operations.
type Adapter struct {
	rt *docker.Runtime
}

// NewAdapter creates a CorrosionRuntime adapter backed by a docker.Runtime.
func NewAdapter(rt *docker.Runtime) *Adapter {
	return &Adapter{rt: rt}
}

func (a *Adapter) WriteConfig(cfg overlay.CorrosionConfig) error {
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

func (a *Adapter) Start(ctx context.Context, cfg overlay.CorrosionConfig) error {
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
