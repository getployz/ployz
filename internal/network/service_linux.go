//go:build linux

package network

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"os"

	"ployz/internal/docker"
	"ployz/internal/wireguard"

	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
	"github.com/vishvananda/netlink"
	"golang.zx2c4.com/wireguard/wgctrl/wgtypes"
)

func New() (*Controller, error) {
	cli, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, fmt.Errorf("create docker client: %w", err)
	}
	return &Controller{cli: cli}, nil
}

type linuxRuntimeOps struct {
	ctrl *Controller
}

func (o linuxRuntimeOps) Prepare(ctx context.Context, _ Config) error {
	return docker.WaitReady(ctx, o.ctrl.cli)
}

func (o linuxRuntimeOps) ConfigureWireGuard(_ context.Context, cfg Config, state *State) error {
	return configureWireGuardLinux(cfg, state, nil)
}

func (o linuxRuntimeOps) EnsureDockerNetwork(ctx context.Context, cfg Config, _ *State) error {
	bridge, err := docker.EnsureNetwork(ctx, o.ctrl.cli, cfg.DockerNetwork, cfg.Subnet, cfg.WGInterface)
	if err != nil {
		return err
	}
	return docker.EnsureIptablesRules(cfg.Subnet, cfg.WGInterface, bridge)
}

func (o linuxRuntimeOps) CleanupDockerNetwork(ctx context.Context, cfg Config, state *State) error {
	bridge, err := docker.CleanupNetwork(ctx, o.ctrl.cli, cfg.DockerNetwork)
	if err != nil {
		return err
	}
	if bridge == "" {
		return nil
	}
	return docker.CleanupIptablesRules(state.Subnet, state.WGInterface, bridge)
}

func (o linuxRuntimeOps) CleanupWireGuard(_ context.Context, _ Config, state *State) error {
	return wireguard.Cleanup(state.WGInterface)
}

func (o linuxRuntimeOps) AfterStop(_ context.Context, _ Config, _ *State) error {
	return nil
}

func (c *Controller) Start(ctx context.Context, in Config) (Config, error) {
	return c.startRuntime(ctx, in, linuxRuntimeOps{ctrl: c})
}

func (c *Controller) Stop(ctx context.Context, in Config, purge bool) (Config, error) {
	return c.stopRuntime(ctx, in, purge, linuxRuntimeOps{ctrl: c})
}

func (c *Controller) Status(ctx context.Context, in Config) (Status, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Status{}, err
	}

	out := Status{StatePath: statePath(cfg.DataDir)}
	s, err := loadState(cfg.DataDir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return out, nil
		}
		return Status{}, err
	}
	out.Configured = true
	out.Running = s.Running
	cfg, err = Resolve(cfg, s)
	if err != nil {
		return Status{}, err
	}

	if _, err := netlink.LinkByName(s.WGInterface); err == nil {
		out.WireGuard = true
	}
	if err := docker.WaitReady(ctx, c.cli); err == nil {
		if n, err := c.cli.NetworkInspect(ctx, s.DockerNetwork, dockernetwork.InspectOptions{}); err == nil && n.ID != "" {
			out.DockerNet = true
		}
		if ctr, err := c.cli.ContainerInspect(ctx, s.CorrosionName); err == nil && ctr.State != nil && ctr.State.Running {
			out.Corrosion = true
		}
	}

	return out, nil
}

func (c *Controller) applyPeerConfig(_ context.Context, cfg Config, state *State, peers []Peer) error {
	return configureWireGuardLinux(cfg, state, peers)
}

func configureWireGuardLinux(cfg Config, state *State, peers []Peer) error {
	priv, err := wgtypes.ParseKey(state.WGPrivate)
	if err != nil {
		return fmt.Errorf("parse private key: %w", err)
	}
	specs, err := buildPeerSpecs(peers)
	if err != nil {
		return fmt.Errorf("build peer specs: %w", err)
	}
	wgPeers := make([]wireguard.PeerConfig, len(specs))
	for i, s := range specs {
		wgPeers[i] = wireguard.PeerConfig{
			PublicKey:       s.publicKey,
			Endpoint:        s.endpoint,
			AllowedPrefixes: s.allowedPrefixes,
		}
	}
	localMachineIP := machineIP(cfg.Subnet)
	return wireguard.Configure(
		state.WGInterface, defaultWireGuardMTU, priv, state.WGPort,
		[]netip.Prefix{singleIPPrefix(localMachineIP), singleIPPrefix(cfg.Management)},
		wgPeers, localMachineIP, cfg.Management,
	)
}
