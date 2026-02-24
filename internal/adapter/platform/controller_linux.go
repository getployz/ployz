//go:build linux

package platform

import (
	"context"
	"fmt"
	"net/netip"

	corrosion "ployz/internal/adapter/corrosion/container"
	"ployz/internal/adapter/docker"
	"ployz/internal/adapter/sqlite"
	"ployz/internal/adapter/wireguard"
	"ployz/internal/network"

	"github.com/vishvananda/netlink"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

// NewController creates a Controller wired with Linux-specific dependencies.
func NewController(opts ...network.Option) (*network.Controller, error) {
	rt, err := docker.NewRuntime()
	if err != nil {
		return nil, err
	}
	defaults := []network.Option{
		network.WithContainerRuntime(rt),
		network.WithCorrosionRuntime(corrosion.NewAdapter(rt)),
		network.WithStatusProber(LinuxStatusProber{RT: rt}),
		network.WithStateStore(sqlite.NetworkStateStore{}),
		network.WithClock(network.RealClock{}),
		network.WithPlatformOps(LinuxPlatformOps{RT: rt}),
	}
	return network.New(append(defaults, opts...)...)
}

// LinuxPlatformOps implements network.PlatformOps for Linux.
type LinuxPlatformOps struct {
	RT network.ContainerRuntime
}

func (o LinuxPlatformOps) Prepare(ctx context.Context, cfg network.Config, state network.StateStore) error {
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

func (o LinuxPlatformOps) ConfigureWireGuard(_ context.Context, cfg network.Config, state *network.State) error {
	return configureWireGuardLinux(cfg, state, nil)
}

func (o LinuxPlatformOps) EnsureDockerNetwork(ctx context.Context, cfg network.Config, _ *network.State) error {
	bridge, err := network.EnsureDockerNetwork(ctx, o.RT, cfg.DockerNetwork, cfg.Subnet, cfg.WGInterface)
	if err != nil {
		return err
	}
	return docker.EnsureIptablesRules(cfg.Subnet, cfg.WGInterface, bridge)
}

func (o LinuxPlatformOps) CleanupDockerNetwork(ctx context.Context, cfg network.Config, state *network.State) error {
	bridge, err := network.CleanupDockerNetwork(ctx, o.RT, cfg.DockerNetwork)
	if err != nil {
		return err
	}
	if bridge == "" {
		return nil
	}
	return docker.CleanupIptablesRules(state.Subnet, state.WGInterface, bridge)
}

func (o LinuxPlatformOps) CleanupWireGuard(_ context.Context, _ network.Config, state *network.State) error {
	return wireguard.Cleanup(state.WGInterface)
}

func (o LinuxPlatformOps) AfterStart(_ context.Context, _ network.Config) error {
	return nil
}

func (o LinuxPlatformOps) AfterStop(_ context.Context, _ network.Config, _ *network.State) error {
	return nil
}

func (o LinuxPlatformOps) ApplyPeerConfig(_ context.Context, cfg network.Config, state *network.State, peers []network.Peer) error {
	return configureWireGuardLinux(cfg, state, peers)
}

// LinuxStatusProber implements network.StatusProber for Linux.
type LinuxStatusProber struct {
	RT network.ContainerRuntime
}

func (p LinuxStatusProber) ProbeInfra(ctx context.Context, state *network.State) (wg bool, dockerNet bool, corr bool, err error) {
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

func configureWireGuardLinux(cfg network.Config, state *network.State, peers []network.Peer) error {
	priv, err := wgtypes.ParseKey(state.WGPrivate)
	if err != nil {
		return fmt.Errorf("parse private key: %w", err)
	}
	specs, err := network.BuildPeerSpecs(peers)
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
	localMachineIP := network.MachineIP(cfg.Subnet)
	return wireguard.Configure(
		state.WGInterface, defaultWireGuardMTU, priv, state.WGPort,
		[]netip.Prefix{network.SingleIPPrefix(localMachineIP), network.SingleIPPrefix(cfg.Management)},
		wgPeers, localMachineIP, cfg.Management,
	)
}

var defaultNetworkPrefix = netip.MustParsePrefix("10.210.0.0/16")

const defaultWireGuardMTU = 1280
