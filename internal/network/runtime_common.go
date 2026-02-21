//go:build linux || darwin

package network

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"os"

	"ployz/internal/corrosion"
	"ployz/internal/netutil"
)

type runtimeOps interface {
	Prepare(ctx context.Context, cfg Config) error
	ConfigureWireGuard(ctx context.Context, cfg Config, state *State) error
	EnsureDockerNetwork(ctx context.Context, cfg Config, state *State) error
	CleanupDockerNetwork(ctx context.Context, cfg Config, state *State) error
	CleanupWireGuard(ctx context.Context, cfg Config, state *State) error
	AfterStop(ctx context.Context, cfg Config, state *State) error
}

func (c *Controller) startRuntime(ctx context.Context, in Config, ops runtimeOps) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}
	if err := netutil.EnsureUniqueHostCIDR(cfg.NetworkCIDR, cfg.DataRoot, cfg.Network, defaultNetworkPrefix, func(dataDir string) (string, error) {
		s, err := loadState(dataDir)
		if err != nil {
			return "", err
		}
		return s.CIDR, nil
	}); err != nil {
		return Config{}, err
	}
	if err := ops.Prepare(ctx, cfg); err != nil {
		return Config{}, err
	}

	state, _, err := ensureState(cfg)
	if err != nil {
		return Config{}, err
	}

	if cfg.Subnet.IsValid() && state.Subnet != cfg.Subnet.String() {
		return Config{}, fmt.Errorf("network %q already initialized with subnet %s", cfg.Network, state.Subnet)
	}
	if cfg.NetworkCIDR.IsValid() && state.CIDR != "" && state.CIDR != cfg.NetworkCIDR.String() {
		return Config{}, fmt.Errorf("network %q already initialized with cidr %s", cfg.Network, state.CIDR)
	}
	if cfg.AdvertiseEP != "" && cfg.AdvertiseEP != state.Advertise {
		state.Advertise = cfg.AdvertiseEP
	}
	if cfg.WGPort != 0 && state.WGPort != cfg.WGPort {
		state.WGPort = cfg.WGPort
	}
	if len(cfg.CorrosionBootstrap) > 0 {
		state.Bootstrap = append([]string(nil), cfg.CorrosionBootstrap...)
	}
	if cfg.NetworkCIDR.IsValid() {
		state.CIDR = cfg.NetworkCIDR.String()
	}

	cfg, err = Resolve(cfg, state)
	if err != nil {
		return Config{}, err
	}

	if err := ops.ConfigureWireGuard(ctx, cfg, state); err != nil {
		return Config{}, err
	}
	if err := corrosion.WriteConfig(corrosion.Config{
		Dir:          cfg.CorrosionDir,
		SchemaPath:   cfg.CorrosionSchema,
		ConfigPath:   cfg.CorrosionConfig,
		AdminSock:    cfg.CorrosionAdminSock,
		Bootstrap:    cfg.CorrosionBootstrap,
		GossipAddr:   cfg.CorrosionGossipAP,
		APIAddr:      cfg.CorrosionAPIAddr,
		GossipMaxMTU: corrosionGossipMaxMTU(cfg.CorrosionGossipIP),
		User:         cfg.CorrosionUser,
	}); err != nil {
		return Config{}, err
	}
	if err := corrosion.Start(ctx, c.cli, corrosion.RuntimeConfig{
		Name:       cfg.CorrosionName,
		Image:      cfg.CorrosionImg,
		ConfigPath: cfg.CorrosionConfig,
		DataDir:    cfg.CorrosionDir,
		User:       cfg.CorrosionUser,
		APIAddr:    cfg.CorrosionAPIAddr,
	}); err != nil {
		return Config{}, err
	}
	if err := ops.EnsureDockerNetwork(ctx, cfg, state); err != nil {
		return Config{}, err
	}

	state.Running = true
	if err := saveState(cfg.DataDir, state); err != nil {
		return Config{}, err
	}

	return cfg, nil
}

func corrosionGossipMaxMTU(addr netip.Addr) int {
	const udpHeaderLen = 8
	if addr.Is4() {
		return defaultWireGuardMTU - net.IPv4len - udpHeaderLen
	}
	return defaultWireGuardMTU - net.IPv6len - udpHeaderLen
}

func (c *Controller) stopRuntime(ctx context.Context, in Config, purge bool, ops runtimeOps) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}

	state, err := loadState(cfg.DataDir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return cfg, nil
		}
		return Config{}, err
	}

	if err := ops.Prepare(ctx, cfg); err != nil {
		return Config{}, err
	}

	cfg, err = Resolve(cfg, state)
	if err != nil {
		return Config{}, err
	}

	if err := ops.CleanupDockerNetwork(ctx, cfg, state); err != nil {
		return Config{}, err
	}
	if err := corrosion.Stop(ctx, c.cli, state.CorrosionName); err != nil {
		return Config{}, err
	}
	if err := ops.CleanupWireGuard(ctx, cfg, state); err != nil {
		return Config{}, err
	}
	if err := ops.AfterStop(ctx, cfg, state); err != nil {
		return Config{}, err
	}

	state.Running = false
	if purge {
		if err := deleteState(cfg.DataDir); err != nil {
			return Config{}, err
		}
		if err := os.RemoveAll(cfg.DataDir); err != nil {
			return Config{}, fmt.Errorf("purge data dir: %w", err)
		}
	} else if err := saveState(cfg.DataDir, state); err != nil {
		return Config{}, err
	}

	return cfg, nil
}
