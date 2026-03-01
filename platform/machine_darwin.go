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

// NewMachine creates a production machine for macOS. The mesh builder
// wires containerized WireGuard, Corrosion, and convergence.
func NewMachine(dataDir string) (*machine.Machine, error) {
	docker, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, fmt.Errorf("create docker client: %w", err)
	}

	return machine.New(dataDir, machine.WithMeshBuilder(func(ctx context.Context, id machine.Identity) (*mesh.Mesh, error) {
		return buildMesh(id, dataDir, docker)
	}))
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

	runtime := corrorun.NewContainer(docker, paths, apiAddr,
		corrorun.WithImage(CorrosionImage),
		corrorun.WithContainerName(CorrosionContainerName),
		corrorun.WithNetworkMode(container.NetworkMode("container:"+WireGuardContainerName)),
	)

	corroClient, err := corrosion.NewClient(apiAddr)
	if err != nil {
		return nil, fmt.Errorf("create corrosion client: %w", err)
	}

	st := store.New(runtime, corroClient)

	pub := id.PrivateKey.PublicKey()
	self := ployz.MachineRecord{
		ID:        pub.String(),
		Name:      id.Name,
		PublicKey:  pub,
		OverlayIP: mgmtIP,
	}
	conv := convergence.New(self, convergence.MeshPlanner{}, st, wg)

	return mesh.New(
		mesh.WithWireGuard(wg),
		mesh.WithStore(st),
		mesh.WithConvergence(conv),
	), nil
}
