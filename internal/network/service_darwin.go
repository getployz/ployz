//go:build darwin

package network

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/netip"
	"os"

	"ployz/internal/adapter/docker"
	"ployz/internal/adapter/wireguard"
	"ployz/pkg/sdk/defaults"

	dockernetwork "github.com/docker/docker/api/types/network"
	"github.com/docker/docker/client"
)

func New(opts ...Option) (*Controller, error) {
	cli, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, fmt.Errorf("create docker client: %w", err)
	}
	c := &Controller{cli: cli, state: sqliteStateStore{}}
	for _, o := range opts {
		o(c)
	}
	return c, nil
}

type darwinRuntimeOps struct {
	ctrl *Controller
}

func (o darwinRuntimeOps) Prepare(ctx context.Context, _ Config) error {
	return docker.WaitReady(ctx, o.ctrl.cli)
}

func (o darwinRuntimeOps) ConfigureWireGuard(ctx context.Context, cfg Config, state *State) error {
	return configureWireGuardDarwin(ctx, cfg, state, nil)
}

func (o darwinRuntimeOps) EnsureDockerNetwork(_ context.Context, _ Config, _ *State) error {
	return nil // no containers on the overlay on macOS
}

func (o darwinRuntimeOps) CleanupDockerNetwork(_ context.Context, _ Config, _ *State) error {
	return nil
}

func (o darwinRuntimeOps) CleanupWireGuard(ctx context.Context, _ Config, _ *State) error {
	return wireguard.Cleanup(ctx)
}

func (o darwinRuntimeOps) AfterStop(_ context.Context, _ Config, _ *State) error {
	return nil
}

func (c *Controller) Start(ctx context.Context, in Config) (Config, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Config{}, err
	}
	out, err := c.startRuntime(ctx, in, darwinRuntimeOps{ctrl: c})
	if err != nil {
		return Config{}, err
	}

	// Start a TCP ping listener on the overlay IP so remote nodes can measure
	// latency. With userspace WireGuard on the host we can bind directly.
	overlayIP := machineIP(out.Subnet)
	pingPort := defaults.DaemonAPIPort(out.Network)
	go startPingListener(ctx, overlayIP, pingPort, out.Network)

	_ = cfg // used only for NormalizeConfig above
	return out, nil
}

func (c *Controller) Stop(ctx context.Context, in Config, purge bool) (Config, error) {
	return c.stopRuntime(ctx, in, purge, darwinRuntimeOps{ctrl: c})
}

func (c *Controller) Status(ctx context.Context, in Config) (Status, error) {
	cfg, err := NormalizeConfig(in)
	if err != nil {
		return Status{}, err
	}

	out := Status{StatePath: statePath(cfg.DataDir)}
	s, err := c.state.Load(cfg.DataDir)
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

	out.WireGuard = wireguard.IsActive()

	if err := docker.WaitReady(ctx, c.cli); err == nil {
		if n, nErr := c.cli.NetworkInspect(ctx, s.DockerNetwork, dockernetwork.InspectOptions{}); nErr == nil && n.ID != "" {
			out.DockerNet = true
		}
		if ctr, cErr := c.cli.ContainerInspect(ctx, s.CorrosionName); cErr == nil && ctr.State != nil && ctr.State.Running {
			out.Corrosion = true
		}
	}

	return out, nil
}

func (c *Controller) applyPeerConfig(ctx context.Context, cfg Config, state *State, peers []Peer) error {
	return configureWireGuardDarwin(ctx, cfg, state, peers)
}

func configureWireGuardDarwin(ctx context.Context, cfg Config, state *State, peers []Peer) error {
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
	return wireguard.Configure(ctx,
		state.WGInterface, defaultWireGuardMTU, state.WGPrivate, state.WGPort,
		machineIP(cfg.Subnet), cfg.Management, wgPeers)
}

// startPingListener runs a TCP accept loop on the overlay IP so remote peers
// can measure RTT to this node.
func startPingListener(ctx context.Context, ip netip.Addr, port int, network string) {
	var addr string
	if ip.Is4() {
		addr = fmt.Sprintf("%s:%d", ip, port)
	} else {
		addr = fmt.Sprintf("[%s]:%d", ip, port)
	}
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		slog.Warn("ping listener setup failed", "network", network, "addr", addr, "err", err)
		return
	}
	slog.Debug("ping listener started", "network", network, "addr", addr)
	go func() {
		<-ctx.Done()
		_ = ln.Close()
	}()
	for {
		conn, err := ln.Accept()
		if err != nil {
			return // listener closed
		}
		_ = conn.Close()
	}
}
