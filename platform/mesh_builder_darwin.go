//go:build darwin

package platform

import (
	"context"
	"fmt"
	"net/netip"

	"ployz"
	"ployz/infra/corrorun"
	"ployz/infra/corrosion"
	"ployz/infra/store"
	"ployz/machine"
	"ployz/machine/convergence"
	"ployz/machine/mesh"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/client"
)

// NewMeshBuilder returns a builder that wires containerized WireGuard,
// Corrosion, and convergence for macOS. Identity is captured in the closure.
func NewMeshBuilder(id machine.Identity, dataDir string) (machine.MeshBuilder, error) {
	docker, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, fmt.Errorf("create docker client: %w", err)
	}

	return func(ctx context.Context) (machine.NetworkStack, error) {
		return buildMesh(id, dataDir, docker)
	}, nil
}

func buildMesh(id machine.Identity, dataDir string, docker client.APIClient) (*mesh.Mesh, error) {
	wg := NewWireGuard(id.PrivateKey, docker)

	mgmtIP := ployz.ManagementIPFromKey(id.PrivateKey.PublicKey())
	gossipAddr := netip.AddrPortFrom(mgmtIP, corrorun.DefaultGossipPort)
	apiAddr := netip.AddrPortFrom(mgmtIP, corrorun.DefaultAPIPort)

	paths := corrorun.NewPaths(dataDir)
	if err := corrorun.WriteConfig(paths, store.Schema, gossipAddr, apiAddr, nil); err != nil {
		return nil, fmt.Errorf("write corrosion config: %w", err)
	}

	// Custom readiness check that dials through the overlay bridge.
	readyCheck := func(ctx context.Context, addr netip.AddrPort) error {
		c, err := corrosion.NewClient(addr, corrosion.WithDialFunc(wg.DialContext))
		if err != nil {
			return fmt.Errorf("create overlay client: %w", err)
		}
		return corrorun.WaitReadyWith(ctx, c)
	}

	runtime := corrorun.NewContainer(docker, paths, apiAddr,
		corrorun.WithImage(CorrosionImage),
		corrorun.WithContainerName(CorrosionContainerName),
		corrorun.WithNetworkMode(container.NetworkMode("container:"+WireGuardContainerName)),
		corrorun.WithReadinessCheck(readyCheck),
	)

	corroClient, err := corrosion.NewClient(apiAddr, corrosion.WithDialFunc(wg.DialContext))
	if err != nil {
		return nil, fmt.Errorf("create corrosion client: %w", err)
	}

	st := store.New(runtime, corroClient)

	pub := id.PrivateKey.PublicKey()
	self := ployz.MachineRecord{
		ID:        pub.String(),
		Name:      id.Name,
		PublicKey: pub,
		OverlayIP: mgmtIP,
	}
	conv := convergence.New(self,
		convergence.WithSubscriber(st),
		convergence.WithPeerSetter(wg),
		convergence.WithProber(wg),
	)

	return mesh.New(
		mesh.WithWireGuard(wg),
		mesh.WithStore(st),
		mesh.WithConvergence(conv),
		mesh.WithStoreHealth(st),
		mesh.WithOverlayNet(wg),
	), nil
}
