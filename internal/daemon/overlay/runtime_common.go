package overlay

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/netip"
	"os"
	"strings"
)

func (c *Controller) Start(ctx context.Context, in Config) (Config, error) {
	out, err := c.startRuntime(ctx, in)
	if err != nil {
		return Config{}, err
	}
	if err := c.platformOps.AfterStart(ctx, out); err != nil {
		return Config{}, fmt.Errorf("after-start hook: %w", err)
	}
	return out, nil
}

func (c *Controller) Stop(ctx context.Context, in Config, purge bool) (Config, error) {
	return c.stopRuntime(ctx, in, purge)
}

func (c *Controller) startRuntime(ctx context.Context, in Config) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}
	log := slog.With("component", "network-runtime", "network", cfg.Network)
	log.Info("start requested")

	if err := c.platformOps.Prepare(ctx, cfg, c.state); err != nil {
		log.Error("prepare failed", "err", err)
		return Config{}, err
	}

	state, _, err := ensureState(c.state, cfg)
	if err != nil {
		return Config{}, err
	}

	if cfg.Subnet.IsValid() && state.Subnet != cfg.Subnet.String() {
		return Config{}, fmt.Errorf("network %q already initialized with subnet %s", cfg.Network, state.Subnet)
	}
	if cfg.NetworkCIDR.IsValid() && state.CIDR != "" && state.CIDR != cfg.NetworkCIDR.String() {
		return Config{}, fmt.Errorf("network %q already initialized with cidr %s", cfg.Network, state.CIDR)
	}
	if cfg.AdvertiseEndpoint != "" && cfg.AdvertiseEndpoint != state.Advertise {
		state.Advertise = cfg.AdvertiseEndpoint
	}
	if cfg.WGPort != 0 && state.WGPort != cfg.WGPort {
		state.WGPort = cfg.WGPort
	}
	if cfg.NetworkCIDR.IsValid() {
		state.CIDR = cfg.NetworkCIDR.String()
	}
	if cfg.CorrosionMemberID != 0 {
		if state.CorrosionMemberID != 0 && cfg.CorrosionMemberID != state.CorrosionMemberID {
			return Config{}, fmt.Errorf("network %q already initialized with corrosion member id %d", cfg.Network, state.CorrosionMemberID)
		}
		state.CorrosionMemberID = cfg.CorrosionMemberID
	}
	if cfg.CorrosionAPIToken != "" {
		if strings.TrimSpace(state.CorrosionAPIToken) != "" && cfg.CorrosionAPIToken != state.CorrosionAPIToken {
			return Config{}, fmt.Errorf("network %q already initialized with different corrosion api token", cfg.Network)
		}
		state.CorrosionAPIToken = cfg.CorrosionAPIToken
	}
	if state.CorrosionMemberID == 0 || strings.TrimSpace(state.CorrosionAPIToken) == "" {
		memberID, apiToken, err := ensureCorrosionSecurity(state.CorrosionMemberID, state.CorrosionAPIToken)
		if err != nil {
			return Config{}, err
		}
		state.CorrosionMemberID = memberID
		state.CorrosionAPIToken = apiToken
	}

	cfg, err = Resolve(cfg, state)
	if err != nil {
		return Config{}, err
	}
	if len(cfg.CorrosionBootstrap) == 0 {
		state.Bootstrap = nil
	} else {
		state.Bootstrap = append([]string(nil), cfg.CorrosionBootstrap...)
	}
	state.Phase = state.Phase.Transition(NetworkStarting)

	if err := c.platformOps.ConfigureWireGuard(ctx, cfg, state); err != nil {
		log.Error("wireguard configure failed", "err", err)
		return Config{}, err
	}
	log.Debug("wireguard configured", "iface", state.WGInterface)
	corrosionCfg := CorrosionConfig{
		Name:         cfg.CorrosionName,
		Image:        cfg.CorrosionImage,
		Dir:          cfg.CorrosionDir,
		ConfigPath:   cfg.CorrosionConfig,
		DataDir:      cfg.CorrosionDir,
		AdminSock:    cfg.CorrosionAdminSock,
		Bootstrap:    cfg.CorrosionBootstrap,
		GossipAddr:   cfg.CorrosionGossipAddrPort,
		MemberID:     cfg.CorrosionMemberID,
		APIAddr:      cfg.CorrosionAPIAddr,
		APIToken:     cfg.CorrosionAPIToken,
		GossipMaxMTU: corrosionGossipMaxMTU(cfg.CorrosionGossipIP),
		User:         cfg.CorrosionUser,
	}
	if err := c.corrosion.WriteConfig(corrosionCfg); err != nil {
		log.Error("write corrosion config failed", "err", err)
		return Config{}, err
	}
	if err := c.corrosion.Start(ctx, corrosionCfg); err != nil {
		log.Error("start corrosion failed", "err", err)
		return Config{}, err
	}
	if err := c.platformOps.EnsureDockerNetwork(ctx, cfg, state); err != nil {
		log.Error("ensure docker network failed", "err", err)
		return Config{}, err
	}

	state.Phase = state.Phase.Transition(NetworkRunning)
	if err := c.state.Save(cfg.DataDir, state); err != nil {
		return Config{}, err
	}

	log.Info("start complete", "subnet", cfg.Subnet.String(), "management_ip", cfg.Management.String())

	return cfg, nil
}

func corrosionGossipMaxMTU(addr netip.Addr) int {
	const udpHeaderLen = 8
	if addr.Is4() {
		return defaultWireGuardMTU - net.IPv4len - udpHeaderLen
	}
	return defaultWireGuardMTU - net.IPv6len - udpHeaderLen
}

func (c *Controller) stopRuntime(ctx context.Context, in Config, purge bool) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}
	log := slog.With("component", "network-runtime", "network", cfg.Network, "purge", purge)
	log.Info("stop requested")

	state, err := c.state.Load(cfg.DataDir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return cfg, nil
		}
		return Config{}, err
	}

	if err := c.platformOps.Prepare(ctx, cfg, c.state); err != nil {
		log.Error("prepare failed", "err", err)
		return Config{}, err
	}

	cfg, err = Resolve(cfg, state)
	if err != nil {
		return Config{}, err
	}
	state.Phase = state.Phase.Transition(NetworkStopping)

	if err := c.platformOps.CleanupDockerNetwork(ctx, cfg, state); err != nil {
		log.Error("cleanup docker network failed", "err", err)
		return Config{}, err
	}
	if err := c.corrosion.Stop(ctx, state.CorrosionName); err != nil {
		log.Error("stop corrosion failed", "err", err)
		return Config{}, err
	}
	if err := c.platformOps.CleanupWireGuard(ctx, cfg, state); err != nil {
		log.Error("cleanup wireguard failed", "err", err)
		return Config{}, err
	}
	if err := c.platformOps.AfterStop(ctx, cfg, state); err != nil {
		log.Error("after-stop hook failed", "err", err)
		return Config{}, err
	}

	if purge {
		state.Phase = state.Phase.Transition(NetworkPurged)
	} else {
		state.Phase = state.Phase.Transition(NetworkStopped)
	}
	if purge {
		if err := c.state.Delete(cfg.DataDir); err != nil {
			return Config{}, err
		}
		if err := os.RemoveAll(cfg.DataDir); err != nil {
			return Config{}, fmt.Errorf("purge data dir: %w", err)
		}
	} else if err := c.state.Save(cfg.DataDir, state); err != nil {
		return Config{}, err
	}

	log.Info("stop complete")

	return cfg, nil
}
