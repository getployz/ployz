//go:build linux

package platform

import (
	"context"
	"fmt"
	"net/netip"

	"ployz"
	"ployz/infra/corrorun"
	"ployz/infra/corrosion"
	"ployz/infra/overlay"
	"ployz/infra/store"
	"ployz/machine"
	"ployz/machine/convergence"
	"ployz/machine/mesh"
)

// NewMachine creates a production machine for Linux. The mesh builder
// wires kernel WireGuard, a Corrosion child process, and convergence.
func NewMachine(dataDir string) (*machine.Machine, error) {
	return machine.New(dataDir, machine.WithMeshBuilder(func(ctx context.Context, id machine.Identity) (*mesh.Mesh, error) {
		return buildMesh(id, dataDir)
	}))
}

func buildMesh(id machine.Identity, dataDir string) (*mesh.Mesh, error) {
	wg := NewWireGuard(id.PrivateKey)

	mgmtIP := ployz.ManagementIPFromKey(id.PrivateKey.PublicKey())
	gossipAddr := netip.AddrPortFrom(mgmtIP, corrorun.DefaultGossipPort)
	apiAddr := netip.AddrPortFrom(mgmtIP, corrorun.DefaultAPIPort)

	paths := corrorun.NewPaths(dataDir)
	if err := corrorun.WriteConfig(paths, store.Schema, gossipAddr, apiAddr, nil); err != nil {
		return nil, fmt.Errorf("write corrosion config: %w", err)
	}

	runtime := corrorun.NewExec(paths, apiAddr)

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
	conv := convergence.New(self, convergence.MeshPlanner{}, st, wg, wg)

	return mesh.New(
		mesh.WithWireGuard(wg),
		mesh.WithStore(st),
		mesh.WithConvergence(conv),
		mesh.WithStoreHealth(st),
		mesh.WithOverlayNet(overlay.Host{}),
	), nil
}
