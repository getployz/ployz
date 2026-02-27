//go:build linux

package platform

import (
	"context"
	"fmt"
	"net/netip"

	corrosion "ployz/internal/infra/corrosion/container"
	"ployz/internal/infra/docker"
	"ployz/internal/infra/sqlite"
	"ployz/internal/infra/wireguard"
	"ployz/internal/daemon/overlay"

	"github.com/vishvananda/netlink"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// NewController creates an overlay service wired with Linux-specific dependencies.
func NewController(opts ...overlay.Option) (*overlay.Service, error) {
	rt, err := docker.NewRuntime()
	if err != nil {
		return nil, err
	}
	defaults := []overlay.Option{
		overlay.WithContainerRuntime(rt),
		overlay.WithCorrosionRuntime(corrosion.NewAdapter(rt)),
		overlay.WithStatusProber(LinuxStatusProber{RT: rt}),
		overlay.WithStateStore(sqlite.NetworkStateStore{}),
		overlay.WithClock(overlay.RealClock{}),
		overlay.WithPlatformOps(LinuxPlatformOps{RT: rt}),
	}
	return overlay.NewService(append(defaults, opts...)...)
}

// LinuxPlatformOps implements overlay.PlatformOps for Linux.
type LinuxPlatformOps struct {
	RT overlay.ContainerRuntime
}

func (o LinuxPlatformOps) Prepare(ctx context.Context, cfg overlay.Config, state overlay.StateStore) error {
	if err := EnsureUniqueHostCIDR(cfg.NetworkCIDR, cfg.DataRoot, cfg.Network, defaultNetworkPrefix, func(dataDir string) (string, error) {
		s, err := state.Load(dataDir)
		if err != nil {
			return "", err
		}
		return s.CIDR, nil
	}); err != nil {
		return err
	}
	return o.RT.WaitReady(ctx)
}

func (o LinuxPlatformOps) ConfigureWireGuard(_ context.Context, cfg overlay.Config, state *overlay.State) error {
	return configureWireGuardLinux(cfg, state, nil)
}

func (o LinuxPlatformOps) EnsureDockerNetwork(ctx context.Context, cfg overlay.Config, _ *overlay.State) error {
	bridge, err := overlay.EnsureDockerNetwork(ctx, o.RT, cfg.DockerNetwork, cfg.Subnet, cfg.WGInterface)
	if err != nil {
		return err
	}
	return docker.EnsureIptablesRules(cfg.Subnet, cfg.WGInterface, bridge)
}

func (o LinuxPlatformOps) CleanupDockerNetwork(ctx context.Context, cfg overlay.Config, state *overlay.State) error {
	bridge, err := overlay.CleanupDockerNetwork(ctx, o.RT, cfg.DockerNetwork)
	if err != nil {
		return err
	}
	if bridge == "" {
		return nil
	}
	return docker.CleanupIptablesRules(state.Subnet, state.WGInterface, bridge)
}

func (o LinuxPlatformOps) CleanupWireGuard(_ context.Context, _ overlay.Config, state *overlay.State) error {
	return wireguard.Cleanup(state.WGInterface)
}

func (o LinuxPlatformOps) AfterStart(_ context.Context, _ overlay.Config) error {
	return nil
}

func (o LinuxPlatformOps) AfterStop(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return nil
}

func (o LinuxPlatformOps) ApplyPeerConfig(_ context.Context, cfg overlay.Config, state *overlay.State, peers []overlay.Peer) error {
	return configureWireGuardLinux(cfg, state, peers)
}

// LinuxStatusProber implements overlay.StatusProber for Linux.
type LinuxStatusProber struct {
	RT overlay.ContainerRuntime
}

func (p LinuxStatusProber) ProbeInfra(ctx context.Context, state *overlay.State) (wg bool, dockerNet bool, corr bool, err error) {
	if _, linkErr := netlink.LinkByName(state.WGInterface); linkErr == nil {
		wg = true
	}
	if err := p.RT.WaitReady(ctx); err == nil {
		if nw, nErr := p.RT.NetworkInspect(ctx, state.DockerNetwork); nErr == nil && nw.Exists {
			dockerNet = true
		}
		if ctr, cErr := p.RT.ContainerInspect(ctx, state.CorrosionName); cErr == nil && ctr.Running {
			corr = true
		}
	}
	return wg, dockerNet, corr, nil
}

func configureWireGuardLinux(cfg overlay.Config, state *overlay.State, peers []overlay.Peer) error {
	priv, err := wgtypes.ParseKey(state.WGPrivate)
	if err != nil {
		return fmt.Errorf("parse private key: %w", err)
	}
	specs, err := overlay.BuildPeerSpecs(peers)
	if err != nil {
		return fmt.Errorf("build peer specs: %w", err)
	}
	wgPeers := make([]wireguard.PeerConfig, len(specs))
	for i, s := range specs {
		wgPeers[i] = wireguard.PeerConfig{
			PublicKey:       s.PublicKey,
			Endpoint:        s.Endpoint,
			AllowedPrefixes: s.AllowedPrefixes,
		}
	}
	localMachineIP := overlay.MachineIP(cfg.Subnet)
	return wireguard.Configure(
		state.WGInterface, defaultWireGuardMTU, priv, state.WGPort,
		[]netip.Prefix{overlay.SingleIPPrefix(localMachineIP), overlay.SingleIPPrefix(cfg.Management)},
		wgPeers, localMachineIP, cfg.Management,
	)
}

var defaultNetworkPrefix = netip.MustParsePrefix("10.210.0.0/16")

const defaultWireGuardMTU = 1280
