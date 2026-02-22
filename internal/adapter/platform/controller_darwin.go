//go:build darwin

package platform

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"net/netip"

	corrosion "ployz/internal/adapter/corrosion/container"
	"ployz/internal/adapter/docker"
	"ployz/internal/adapter/sqlite"
	"ployz/internal/adapter/wireguard"
	"ployz/internal/mesh"
	"ployz/pkg/sdk/defaults"
)

// NewController creates a Controller wired with Darwin-specific dependencies.
func NewController(opts ...mesh.Option) (*mesh.Controller, error) {
	rt, err := docker.NewRuntime()
	if err != nil {
		return nil, err
	}
	defaults := []mesh.Option{
		mesh.WithContainerRuntime(rt),
		mesh.WithCorrosionRuntime(corrosion.NewAdapter(rt)),
		mesh.WithStatusProber(DarwinStatusProber{RT: rt}),
		mesh.WithStateStore(sqlite.NetworkStateStore{}),
		mesh.WithClock(mesh.RealClock{}),
		mesh.WithPlatformOps(DarwinPlatformOps{RT: rt}),
	}
	return mesh.New(append(defaults, opts...)...)
}

// DarwinPlatformOps implements mesh.PlatformOps for macOS.
type DarwinPlatformOps struct {
	RT mesh.ContainerRuntime
}

func (o DarwinPlatformOps) Prepare(ctx context.Context, cfg mesh.Config, state mesh.StateStore) error {
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

func (o DarwinPlatformOps) ConfigureWireGuard(ctx context.Context, cfg mesh.Config, state *mesh.State) error {
	return configureWireGuardDarwin(ctx, cfg, state, nil)
}

func (o DarwinPlatformOps) EnsureDockerNetwork(_ context.Context, _ mesh.Config, _ *mesh.State) error {
	return nil // no containers on the overlay on macOS
}

func (o DarwinPlatformOps) CleanupDockerNetwork(_ context.Context, _ mesh.Config, _ *mesh.State) error {
	return nil
}

func (o DarwinPlatformOps) CleanupWireGuard(ctx context.Context, _ mesh.Config, _ *mesh.State) error {
	return wireguard.Cleanup(ctx)
}

func (o DarwinPlatformOps) AfterStart(ctx context.Context, cfg mesh.Config) error {
	// Start a TCP ping listener on the overlay IP so remote nodes can measure
	// latency. With userspace WireGuard on the host we can bind directly.
	overlayIP := mesh.MachineIP(cfg.Subnet)
	pingPort := defaults.DaemonAPIPort(cfg.Network)
	go startPingListener(ctx, overlayIP, pingPort, cfg.Network)
	return nil
}

func (o DarwinPlatformOps) AfterStop(_ context.Context, _ mesh.Config, _ *mesh.State) error {
	return nil
}

func (o DarwinPlatformOps) ApplyPeerConfig(ctx context.Context, cfg mesh.Config, state *mesh.State, peers []mesh.Peer) error {
	return configureWireGuardDarwin(ctx, cfg, state, peers)
}

// DarwinStatusProber implements mesh.StatusProber for macOS.
type DarwinStatusProber struct {
	RT mesh.ContainerRuntime
}

func (p DarwinStatusProber) ProbeInfra(ctx context.Context, state *mesh.State) (wg bool, dockerNet bool, corr bool, err error) {
	wg = wireguard.IsActive()

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

func configureWireGuardDarwin(ctx context.Context, cfg mesh.Config, state *mesh.State, peers []mesh.Peer) error {
	specs, err := mesh.BuildPeerSpecs(peers)
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
	return wireguard.Configure(ctx,
		state.WGInterface, defaultWireGuardMTU, state.WGPrivate, state.WGPort,
		mesh.MachineIP(cfg.Subnet), cfg.Management, wgPeers)
}

// startPingListener runs a TCP accept loop on the overlay IP so remote peers
// can measure RTT to this node.
func startPingListener(ctx context.Context, ip netip.Addr, port int, networkName string) {
	var addr string
	if ip.Is4() {
		addr = fmt.Sprintf("%s:%d", ip, port)
	} else {
		addr = fmt.Sprintf("[%s]:%d", ip, port)
	}
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		slog.Warn("ping listener setup failed", "network", networkName, "addr", addr, "err", err)
		return
	}
	slog.Debug("ping listener started", "network", networkName, "addr", addr)
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

var defaultNetworkPrefix = netip.MustParsePrefix("10.210.0.0/16")

const defaultWireGuardMTU = 1280
