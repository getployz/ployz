//go:build darwin

package process

import (
	"context"

	"ployz/internal/adapter/corrosion/container"
	"ployz/internal/network"
)

var _ network.CorrosionRuntime = (*Adapter)(nil)

// Adapter implements network.CorrosionRuntime by running Corrosion as a host process.
type Adapter struct{}

// NewAdapter creates a process-backed Corrosion runtime adapter.
func NewAdapter() *Adapter {
	return &Adapter{}
}

func (a *Adapter) WriteConfig(cfg network.CorrosionConfig) error {
	return container.WriteConfig(container.Config{
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

func (a *Adapter) Start(ctx context.Context, cfg network.CorrosionConfig) error {
	return Start(ctx, RuntimeConfig{
		Name:       cfg.Name,
		ConfigPath: cfg.ConfigPath,
		DataDir:    cfg.DataDir,
		GossipAddr: cfg.GossipAddr,
		APIAddr:    cfg.APIAddr,
		APIToken:   cfg.APIToken,
	})
}

func (a *Adapter) Stop(ctx context.Context, name string) error {
	return Stop(ctx, name)
}
