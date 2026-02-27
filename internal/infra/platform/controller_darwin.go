//go:build darwin

package platform

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"net/netip"
	"time"

	"ployz/internal/daemon/overlay"
	corrosion "ployz/internal/infra/corrosion"
	corrosionprocess "ployz/internal/infra/corrosion/process"
	"ployz/internal/infra/docker"
	"ployz/internal/infra/sqlite"
	"ployz/internal/infra/wireguard"
	"ployz/pkg/sdk/defaults"
)

// NewController creates an overlay service wired with Darwin-specific dependencies.
func NewController(opts ...overlay.Option) (*overlay.Service, error) {
	rt, err := docker.NewRuntime()
	if err != nil {
		return nil, err
	}
	defaults := []overlay.Option{
		overlay.WithContainerRuntime(rt),
		overlay.WithCorrosionRuntime(corrosionprocess.NewAdapter()),
		overlay.WithStatusProber(DarwinStatusProber{RT: rt}),
		overlay.WithStateStore(sqlite.NetworkStateStore{}),
		overlay.WithClock(overlay.RealClock{}),
		overlay.WithPlatformOps(DarwinPlatformOps{RT: rt}),
	}
	return overlay.NewService(append(defaults, opts...)...)
}

// DarwinPlatformOps implements overlay.PlatformOps for macOS.
type DarwinPlatformOps struct {
	RT overlay.ContainerRuntime
}

func (o DarwinPlatformOps) Prepare(ctx context.Context, cfg overlay.Config, state overlay.StateStore) error {
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

func (o DarwinPlatformOps) ConfigureWireGuard(ctx context.Context, cfg overlay.Config, state *overlay.State) error {
	return configureWireGuardDarwin(ctx, cfg, state, nil)
}

func (o DarwinPlatformOps) EnsureDockerNetwork(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return nil // no containers on the overlay on macOS
}

func (o DarwinPlatformOps) CleanupDockerNetwork(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return nil
}

func (o DarwinPlatformOps) CleanupWireGuard(ctx context.Context, _ overlay.Config, _ *overlay.State) error {
	return wireguard.Cleanup(ctx)
}

func (o DarwinPlatformOps) AfterStart(ctx context.Context, cfg overlay.Config) error {
	// Start a TCP ping listener on the overlay IP so remote nodes can measure
	// latency. With userspace WireGuard on the host we can bind directly.
	overlayIP := overlay.MachineIP(cfg.Subnet)
	pingPort := defaults.DaemonAPIPort(cfg.Network)
	go startPingListener(ctx, overlayIP, pingPort, cfg.Network)
	return nil
}

func (o DarwinPlatformOps) AfterStop(_ context.Context, _ overlay.Config, _ *overlay.State) error {
	return nil
}

func (o DarwinPlatformOps) ApplyPeerConfig(ctx context.Context, cfg overlay.Config, state *overlay.State, peers []overlay.Peer) error {
	return configureWireGuardDarwin(ctx, cfg, state, peers)
}

// DarwinStatusProber implements overlay.StatusProber for macOS.
type DarwinStatusProber struct {
	RT overlay.ContainerRuntime
}

func (p DarwinStatusProber) ProbeInfra(ctx context.Context, state *overlay.State, expectedCorrosionMembers int) (wg bool, dockerNet bool, corr bool, err error) {
	wg = wireguard.IsActive()
	if state == nil {
		return wg, false, false, nil
	}

	if err := p.RT.WaitReady(ctx); err == nil {
		// macOS does not create/require the Linux overlay network.
		dockerNet = true
	}

	apiAddr := netip.AddrPortFrom(netip.MustParseAddr("127.0.0.1"), uint16(defaults.CorrosionAPIPort(state.Network)))
	probeCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
	corr = corrosion.ProbeHealth(probeCtx, apiAddr, state.CorrosionAPIToken, expectedCorrosionMembers) == corrosion.HealthReady
	cancel()

	return wg, dockerNet, corr, nil
}

func configureWireGuardDarwin(ctx context.Context, cfg overlay.Config, state *overlay.State, peers []overlay.Peer) error {
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
	return wireguard.Configure(ctx,
		state.WGInterface, defaultWireGuardMTU, state.WGPrivate, state.WGPort,
		overlay.MachineIP(cfg.Subnet), cfg.Management, wgPeers)
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
